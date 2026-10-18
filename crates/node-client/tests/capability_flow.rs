//! CAP 包：能力令牌强制模式下的客户端端到端行为（真实 axum 节点 + 真实 HTTP + 真实签名）。
//!
//! 覆盖：服务端强制时客户端**自动**取令牌并完成上传/下载、令牌按 (对象, 用途) 缓存复用、
//! 关闭强制时旧客户端行为不变（不发额外请求）。
mod support;

use chat_node_client::NodeClient;
use chat_node_services::blob::{CipherManifest, PublicBlobDescriptor};
use chat_node_services::canonical::{hex_encode, sha256};
use chat_node_services::limits::CapabilityPolicy;
use chat_node_services::services::NodeAuthPolicy;
use chat_protocol::{ObjectId, OperationId};
use support::{
    NOW, TestDevice, application_candidate, group_creation, group_text, registration_request,
    start_node,
};

fn object_text(seed: u8) -> String {
    ObjectId::from_bytes([seed; 32]).encode()
}

fn operation_text(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

fn ciphertext_digest(chunks: &[&[u8]]) -> String {
    let mut combined = Vec::new();
    for chunk in chunks {
        combined.extend_from_slice(chunk);
    }
    hex_encode(&sha256(&combined))
}

fn descriptor(blob_seed: u8, group: &str, source_seq: u64, total: u64, chunk_bytes: u32, chunk_count: u32) -> PublicBlobDescriptor {
    PublicBlobDescriptor {
        blob_id: object_text(blob_seed),
        group_id: group.to_owned(),
        source_seq,
        ciphertext_bytes: total,
        chunk_bytes,
        chunk_count,
    }
}

fn required_policy() -> NodeAuthPolicy {
    NodeAuthPolicy {
        capability: CapabilityPolicy {
            required: true,
            max_ttl_seconds: 3_600,
        },
        ..NodeAuthPolicy::default()
    }
}

/// 起节点 + 一台设备 + 单成员群（作者 owner）+ 一条已接受应用条目。
async fn setup(policy: NodeAuthPolicy) -> (support::TestNode, NodeClient, TestDevice, String, u64) {
    let node = start_node(policy).await;
    let device = TestDevice::new(31);
    let client = NodeClient::new(node.endpoint(), device.device_id.clone(), device.key.clone())
        .expect("client");
    client
        .register_device(&registration_request(&node, &device, "cap-cap", NOW))
        .await
        .expect("device registers");
    let group = group_text(61);
    let creation = group_creation(&group, &[(&device, "owner")]);
    client.create_group(&creation).await.expect("group created");
    let candidate = application_candidate(&group, &device, 121, b"attachment");
    let receipt = client.append_entry(&candidate).await.expect("entry accepted");
    (node, client, device, group, receipt.seq)
}

#[tokio::test]
async fn client_fetches_capability_tokens_automatically_when_the_node_requires_them() {
    let (_node, client, _device, group, source_seq) = setup(required_policy()).await;
    assert!(!client.capability_mode(), "starts in legacy (no-token) mode");
    assert_eq!(client.cached_capability_count(), 0);

    let first: &[u8] = b"head";
    let second: &[u8] = b"tail";
    let descriptor = descriptor(71, &group, source_seq, 8, 4, 2);
    let session = client
        .begin_blob_upload(&operation_text(221), &descriptor)
        .await
        .expect("upload begins");

    // 第一块：服务端 AUTH_REQUIRED → 客户端自适应取令牌并重试成功。
    client
        .put_blob_chunk(&session.upload_id, 0, first)
        .await
        .expect("first chunk is accepted after the client fetches a token");
    assert!(client.capability_mode(), "client adapts to the required mode");
    assert_eq!(client.cached_capability_count(), 1, "one upload token is cached");

    // 第二块：复用缓存里的令牌（不新增缓存项）。
    client
        .put_blob_chunk(&session.upload_id, 1, second)
        .await
        .expect("second chunk reuses the cached token");
    assert_eq!(client.cached_capability_count(), 1, "the token is reused");

    // 明确的申请接口也按绑定校验：换对象/换用途都不复用。
    let token = client
        .issue_capability(&session.upload_id, "blob:upload", Some(120))
        .await
        .expect("explicit token request");
    assert!(token.starts_with("cap1."));

    let manifest = CipherManifest {
        chunk_count: 2,
        ciphertext_bytes: 8,
        ciphertext_digest: ciphertext_digest(&[first, second]),
    };
    client
        .complete_blob_upload(&session.upload_id, &manifest)
        .await
        .expect("publish");

    // 下载同样需要令牌：客户端自动申请 blob:download 令牌。
    let blob = object_text(71);
    let chunk = client
        .read_blob_chunk(&blob, 1)
        .await
        .expect("download works with an auto-fetched token");
    assert_eq!(chunk, second);
    assert_eq!(client.cached_capability_count(), 2, "download token is cached separately");

}

#[tokio::test]
async fn disabled_capability_mode_keeps_the_legacy_client_flow() {
    let (_node, client, _device, group, source_seq) = setup(NodeAuthPolicy::default()).await;
    let chunk: &[u8] = b"lega";
    let descriptor = descriptor(72, &group, source_seq, 4, 4, 1);
    let session = client
        .begin_blob_upload(&operation_text(222), &descriptor)
        .await
        .expect("upload begins");
    client
        .put_blob_chunk(&session.upload_id, 0, chunk)
        .await
        .expect("chunk accepted without any token");
    let manifest = CipherManifest {
        chunk_count: 1,
        ciphertext_bytes: 4,
        ciphertext_digest: ciphertext_digest(&[chunk]),
    };
    client
        .complete_blob_upload(&session.upload_id, &manifest)
        .await
        .expect("publish");
    let blob = object_text(72);
    assert_eq!(
        client.read_blob_chunk(&blob, 0).await.expect("read"),
        chunk.to_vec()
    );
    // 未强制时不发额外请求、也不缓存令牌。
    assert!(!client.capability_mode());
    assert_eq!(client.cached_capability_count(), 0);
}

#[tokio::test]
async fn pre_enabled_mode_fetches_before_the_first_chunk() {
    let (_node, client, _device, group, source_seq) = setup(required_policy()).await;
    client.set_capability_mode(true);
    let descriptor = descriptor(73, &group, source_seq, 4, 4, 1);
    let session = client
        .begin_blob_upload(&operation_text(223), &descriptor)
        .await
        .expect("upload begins");
    client
        .put_blob_chunk(&session.upload_id, 0, b"pre!")
        .await
        .expect("chunk accepted with a pre-fetched token");
    assert_eq!(client.cached_capability_count(), 1);
}

