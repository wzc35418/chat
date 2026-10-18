//! 真 TCP 端到端：challenge → 注册 → 建群 → append（含幂等）→ log/head → 信箱 store/read/ack。
//!
//! 服务端是真实的 node-api axum 服务，客户端是 chat-node-client 的公开 API；
//! 不 mock 任何签名、挑战或错误映射。
mod support;

use chat_node_client::{NodeClient, NodeClientErrorKind, NodeEndpoint};
use chat_node_services::auth::ChallengePurpose;
use chat_node_services::limits::{MAX_HTTP_BODY_BYTES, MAX_QUERY_BYTES};
use chat_node_services::services::NodeAuthPolicy;
use support::{
    NOW, TestDevice, TestNode, application_candidate, group_creation, group_text, mailbox_item,
    registration_request, start_node, submission_text,
};

const OWNER_ROLE: &str = "owner";
const MEMBER_ROLE: &str = "member";

fn policy_with_devices(max_devices: usize) -> NodeAuthPolicy {
    NodeAuthPolicy {
        max_devices_per_account: max_devices,
        ..NodeAuthPolicy::default()
    }
}

struct Flow {
    node: TestNode,
    client_a: NodeClient,
    client_b: NodeClient,
    device_a: TestDevice,
    device_b: TestDevice,
    group: String,
    first_seq: u64,
}

/// 起节点 → 两台设备注册 → 两成员群 → 一条已接受日志条目。
async fn setup_flow(policy: NodeAuthPolicy) -> Flow {
    let node = start_node(policy).await;
    let endpoint = node.endpoint();
    let device_a = TestDevice::new(21);
    let device_b = TestDevice::new(22);
    let client_a = NodeClient::new(endpoint.clone(), device_a.device_id.clone(), device_a.key.clone())
        .expect("client a");
    let client_b = NodeClient::new(endpoint.clone(), device_b.device_id.clone(), device_b.key.clone())
        .expect("client b");

    let registration_a = client_a
        .register_device(&registration_request(&node, &device_a, "cap-a", NOW))
        .await
        .expect("device a registers");
    assert_eq!(registration_a.device_id, device_a.device_id);
    client_b
        .register_device(&registration_request(&node, &device_b, "cap-b", NOW))
        .await
        .expect("device b registers");

    let group = group_text(31);
    let creation = group_creation(
        &group,
        &[(&device_a, OWNER_ROLE), (&device_b, MEMBER_ROLE)],
    );
    let registration = client_a.create_group(&creation).await.expect("group created");
    assert_eq!(registration.group_id, group);

    let candidate = application_candidate(&group, &device_a, 41, b"first cipher");
    let receipt = client_a.append_entry(&candidate).await.expect("entry accepted");
    assert_eq!(receipt.seq, 1);

    Flow {
        node,
        client_a,
        client_b,
        device_a,
        device_b,
        group,
        first_seq: receipt.seq,
    }
}

#[tokio::test]
async fn challenge_registration_and_signed_outputs_are_verified() {
    let node = start_node(NodeAuthPolicy::default()).await;
    let device = TestDevice::new(21);
    let client = NodeClient::new(node.endpoint(), device.device_id.clone(), device.key.clone())
        .expect("client");

    // 匿名挑战：必须由固定节点公钥签名，且绑定 node_id 与用途。
    let challenge = client
        .fetch_challenge(ChallengePurpose::DeviceRegistration)
        .await
        .expect("challenge issued");
    assert_eq!(challenge.node_id, node.node_id);
    assert_eq!(challenge.purpose, ChallengePurpose::DeviceRegistration);
    chat_node_services::auth::verify_auth_challenge(&challenge, &node.node_public_key)
        .expect("challenge is signed by the pinned node key");

    // 注册：节点回执必须验签通过，重复注册返回同一份回执。
    let request = registration_request(&node, &device, "cap-21", NOW);
    let registration = client.register_device(&request).await.expect("registered");
    registration
        .verify(&node.node_public_key)
        .expect("registration is node-signed");
    let repeat = client.register_device(&request).await.expect("idempotent");
    assert_eq!(registration, repeat);

    // 本客户端的设备公钥确实是凭据里声明的那把。
    assert_eq!(
        client.device_public_key(),
        request.credential.device_key().expect("credential key")
    );
}

#[tokio::test]
async fn group_log_head_and_idempotent_append_flow() {
    let flow = setup_flow(NodeAuthPolicy::default()).await;

    // 同 submission 同字节重试：拿回同一 seq/回执（客户端重新签名但字节相同）。
    let candidate = application_candidate(&flow.group, &flow.device_a, 41, b"first cipher");
    let retried = flow
        .client_a
        .append_entry(&candidate)
        .await
        .expect("idempotent retry");
    assert_eq!(retried.seq, 1);

    // 第二条。
    let second = application_candidate(&flow.group, &flow.device_a, 42, b"second cipher");
    let receipt = flow.client_a.append_entry(&second).await.expect("second entry");
    assert_eq!(receipt.seq, 2);

    // 成员 b 也能读日志，head 与页内 head 完全一致。
    let page = flow
        .client_b
        .fetch_log(&flow.group, 0, 5)
        .await
        .expect("log page");
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.head.seq, 2);
    assert_eq!(page.entries[0].receipt.seq, 1);
    assert_eq!(page.entries[1].receipt.previous_hash, page.entries[0].receipt.log_hash);

    let head = flow.client_b.get_head(&flow.group).await.expect("head");
    assert_eq!(head, page.head);
    head.verify(&flow.node.node_public_key).expect("signed head");

    // 分页：limit=1 分两页，第二页从第一页末尾继续。
    let first_page = flow.client_a.fetch_log(&flow.group, 0, 1).await.expect("page 1");
    assert_eq!(first_page.entries.len(), 1);
    assert_eq!(first_page.next_after, 1);
    let second_page = flow
        .client_a
        .fetch_log(&flow.group, first_page.next_after, 1)
        .await
        .expect("page 2");
    assert_eq!(second_page.entries.len(), 1);
    assert_eq!(second_page.entries[0].receipt.seq, 2);
}

#[tokio::test]
async fn mailbox_store_read_and_ack_flow() {
    let flow = setup_flow(NodeAuthPolicy::default()).await;

    let item = mailbox_item(
        "item-1",
        &flow.group,
        &flow.device_b,
        flow.first_seq,
        b"welcome cipher",
        NOW,
    );
    let receipt = flow.client_a.store_mailbox_item(&item).await.expect("stored");
    receipt
        .verify(&flow.node.node_public_key)
        .expect("node-signed mailbox receipt");
    assert_eq!(receipt.target_device_id, flow.device_b.device_id);
    // 同 item 同密文重试：同一份回执（不重复占用配额）。
    let repeat = flow.client_a.store_mailbox_item(&item).await.expect("idempotent");
    assert_eq!(receipt, repeat);

    let second = mailbox_item(
        "item-2",
        &flow.group,
        &flow.device_b,
        flow.first_seq,
        b"second cipher",
        NOW,
    );
    flow.client_a.store_mailbox_item(&second).await.expect("stored");

    // 读页：只读到自己的条目，且分页游标可继续。
    let first_page = flow.client_b.read_mailbox_page(None, 1).await.expect("page 1");
    assert_eq!(first_page.items.len(), 1);
    let cursor = first_page.next_cursor.clone().expect("next cursor");
    let second_page = flow
        .client_b
        .read_mailbox_page(Some(&cursor), 1)
        .await
        .expect("page 2");
    assert_eq!(second_page.items.len(), 1);
    assert_ne!(second_page.items[0].item_id, first_page.items[0].item_id);
    // 读不删除：同一游标重复读得到同一批材料。
    let repeat_page = flow
        .client_b
        .read_mailbox_page(Some(&cursor), 1)
        .await
        .expect("page 2 repeat");
    assert_eq!(second_page.items, repeat_page.items);

    // ACK：幂等，只作用于本设备条目。
    let summary = flow
        .client_b
        .ack_mailbox_items(&["item-1".to_owned()])
        .await
        .expect("ack");
    assert_eq!(summary.acknowledged, vec!["item-1".to_owned()]);
    assert_eq!(summary.released_bytes, 14);
    let again = flow
        .client_b
        .ack_mailbox_items(&["item-1".to_owned()])
        .await
        .expect("idempotent ack");
    assert!(again.acknowledged.is_empty());
    assert_eq!(again.already_consumed, vec!["item-1".to_owned()]);
    assert_eq!(again.released_bytes, 0);

    // 越权 ACK 他人条目：显式 403，不泄漏条目是否存在。
    let error = flow
        .client_a
        .ack_mailbox_items(&["item-2".to_owned()])
        .await
        .expect_err("foreign ack rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Permission);
    assert_eq!(error.node_code(), Some("PERMISSION_DENIED"));
    assert_eq!(error.http_status(), Some(403));
}

#[tokio::test]
async fn wrong_device_key_is_rejected_without_retrying() {
    let flow = setup_flow(NodeAuthPolicy::default()).await;
    // 同一 device_id、另一把私钥：签名必然不匹配。
    let impostor = TestDevice::with_impostor_key(21, 99);
    let client = NodeClient::new(
        flow.node.endpoint(),
        impostor.device_id.clone(),
        impostor.key.clone(),
    )
    .expect("client");
    let error = client
        .get_head(&flow.group)
        .await
        .expect_err("wrong signature rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Authentication);
    assert_eq!(error.node_code(), Some("INVALID_SIGNATURE"));
    assert_eq!(error.http_status(), Some(401));
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn replayed_and_expired_challenges_are_rejected() {
    let flow = setup_flow(NodeAuthPolicy::default()).await;
    let head_path = format!("/v1/groups/{}/head", flow.group);

    // 一次性挑战：同一挑战第二次使用必须被节点拒绝（客户端高层方法每次都会重取）。
    let challenge = flow
        .client_a
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect("challenge");
    flow.client_a
        .send_with_challenge(&challenge, "GET", &head_path, "", b"")
        .await
        .expect("first use accepted");
    let error = flow
        .client_a
        .send_with_challenge(&challenge, "GET", &head_path, "", b"")
        .await
        .expect_err("replay rejected");
    assert_eq!(error.node_code(), Some("AUTH_REPLAY"));
    assert_eq!(error.kind(), NodeClientErrorKind::Authentication);

    // 过期挑战：推进节点时钟超过 TTL 后同样的挑战不可再用。
    let challenge = flow
        .client_a
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect("challenge");
    flow.node.clock.advance(120);
    let error = flow
        .client_a
        .send_with_challenge(&challenge, "GET", &head_path, "", b"")
        .await
        .expect_err("expired challenge rejected");
    assert_eq!(error.node_code(), Some("AUTH_EXPIRED"));

    // 高层方法每次重取挑战，因此时间推进后仍然工作。
    let head = flow.client_a.get_head(&flow.group).await.expect("fresh challenge works");
    assert_eq!(head.seq, 1);
}

#[tokio::test]
async fn unknown_device_and_non_member_are_rejected() {
    let flow = setup_flow(policy_with_devices(3)).await;

    // 从未注册的设备：节点侧 INVALID_CREDENTIAL，不是“假装成功”。
    let stranger = TestDevice::new(23);
    let stranger_client = NodeClient::new(
        flow.node.endpoint(),
        stranger.device_id.clone(),
        stranger.key.clone(),
    )
    .expect("client");
    let error = stranger_client
        .get_head(&flow.group)
        .await
        .expect_err("unregistered device rejected");
    assert_eq!(error.node_code(), Some("INVALID_CREDENTIAL"));
    assert_eq!(error.kind(), NodeClientErrorKind::Authentication);

    // 已注册但不在群内：历史/成员窗口外，显式 403。
    stranger_client
        .register_device(&registration_request(&flow.node, &stranger, "cap-23", NOW))
        .await
        .expect("third device registers");
    let error = stranger_client
        .fetch_log(&flow.group, 0, 5)
        .await
        .expect_err("non member rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Permission);
    assert_eq!(error.node_code(), Some("HISTORY_NOT_AUTHORIZED"));
}

#[tokio::test]
async fn divergent_bytes_for_one_submission_are_a_conflict() {
    let flow = setup_flow(NodeAuthPolicy::default()).await;
    let divergent = application_candidate(&flow.group, &flow.device_a, 41, b"tampered cipher");
    assert_eq!(divergent.submission_id, submission_text(41));
    let error = flow
        .client_a
        .append_entry(&divergent)
        .await
        .expect_err("divergent bytes rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Conflict);
    assert_eq!(error.node_code(), Some("SUBMISSION_ID_CONFLICT"));
    assert_eq!(error.http_status(), Some(409));
}

#[tokio::test]
async fn wrong_node_identity_and_wrong_pinned_key_are_rejected() {
    let flow = setup_flow(NodeAuthPolicy::default()).await;

    // 端点声明的 node_id 与真实节点不同：挑战里的 node_id 不匹配即失败。
    let wrong_id_endpoint = flow.node.unpinned_endpoint("node-other");
    let device = TestDevice::new(21);
    let client = NodeClient::new(wrong_id_endpoint, device.device_id.clone(), device.key.clone())
        .expect("client");
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("wrong node id rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Protocol);
    assert_eq!(error.code(), chat_protocol::ErrorCode::InvalidTarget);

    // 固定的节点公钥与真实节点不同：挑战验签失败。
    let wrong_key_endpoint = NodeEndpoint::new(
        &format!("http://{}", flow.node.address),
        &flow.node.node_id,
    )
    .expect("endpoint")
    .with_node_public_key([9_u8; 32]);
    let client = NodeClient::new(wrong_key_endpoint, device.device_id.clone(), device.key.clone())
        .expect("client");
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("wrong pinned key rejected");
    assert_eq!(error.kind(), NodeClientErrorKind::Protocol);
    assert_eq!(error.code(), chat_protocol::ErrorCode::InvalidSignature);
}
#[tokio::test]
async fn oversized_request_is_rejected_before_sending() {
    let node = start_node(NodeAuthPolicy::default()).await;
    let device = TestDevice::new(21);
    let limits = chat_node_client::ClientLimits {
        max_request_bytes: 16,
        ..chat_node_client::ClientLimits::default()
    };
    let client = NodeClient::with_limits(
        node.endpoint(),
        device.device_id.clone(),
        device.key.clone(),
        limits,
    )
    .expect("client");
    // 挑战请求正文本身就超过 16 字节：客户端在发请求之前显式拒绝，不浪费一次往返。
    let error = client
        .fetch_challenge(ChallengePurpose::RequestAuth)
        .await
        .expect_err("oversized request refused locally");
    assert_eq!(error.kind(), NodeClientErrorKind::Limit);
    // 默认上限必须放得下节点允许的请求正文与最大查询串（游标可能在查询串里）。
    let defaults = chat_node_client::ClientLimits::default();
    assert_eq!(defaults.max_request_bytes, MAX_HTTP_BODY_BYTES);
    assert!(MAX_QUERY_BYTES <= defaults.max_response_bytes);
}
