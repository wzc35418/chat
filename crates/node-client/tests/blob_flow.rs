//! NODE-012~015 附件 blob 的客户端端到端测试（真实 axum 节点 + 真实 HTTP + 真实签名）。
//!
//! 覆盖：上传会话/分块/发布/读取往返、重复块幂等、乱序与缺块拒绝、摘要不符拒绝、
//! 尺寸上限拒绝、未发布不可读、非成员越权读取拒绝。
mod support;

use chat_node_client::{NodeClient, NodeClientErrorKind};
use chat_node_services::blob::{CipherManifest, PublicBlobDescriptor};
use chat_node_services::canonical::{hex_encode, sha256};
use chat_node_services::services::NodeAuthPolicy;
use chat_protocol::{ErrorCode, ObjectId, OperationId};
use support::{
    NOW, TestDevice, application_candidate, device_text, group_creation, group_text,
    registration_request, start_node,
};

const OWNER_ROLE: &str = "owner";
const MEMBER_ROLE: &str = "member";

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

fn descriptor(
    blob_seed: u8,
    group: &str,
    source_seq: u64,
    total: u64,
    chunk_bytes: u32,
    chunk_count: u32,
) -> PublicBlobDescriptor {
    PublicBlobDescriptor {
        blob_id: object_text(blob_seed),
        group_id: group.to_owned(),
        source_seq,
        ciphertext_bytes: total,
        chunk_bytes,
        chunk_count,
    }
}

struct Flow {
    node: support::TestNode,
    owner: NodeClient,
    owner_device: TestDevice,
    member: NodeClient,
    outsider: NodeClient,
    group: String,
    source_seq: u64,
}

/// 起节点 → 注册三台设备 → 两成员群 → 一条已接受日志条目（作者=owner）。
async fn setup(mut policy: NodeAuthPolicy) -> Flow {
    // 三台设备：owner、群成员、群外设备（越权读取用）。
    policy.max_devices_per_account = 3;
    let node = start_node(policy).await;
    let owner_device = TestDevice::new(21);
    let member_device = TestDevice::new(22);
    let outsider_device = TestDevice::new(23);
    let owner = NodeClient::new(
        node.endpoint(),
        owner_device.device_id.clone(),
        owner_device.key.clone(),
    )
    .expect("owner client");
    let member = NodeClient::new(
        node.endpoint(),
        member_device.device_id.clone(),
        member_device.key.clone(),
    )
    .expect("member client");
    let outsider = NodeClient::new(
        node.endpoint(),
        outsider_device.device_id.clone(),
        outsider_device.key.clone(),
    )
    .expect("outsider client");
    owner
        .register_device(&registration_request(&node, &owner_device, "cap-o", NOW))
        .await
        .expect("owner registers");
    member
        .register_device(&registration_request(&node, &member_device, "cap-m", NOW))
        .await
        .expect("member registers");
    outsider
        .register_device(&registration_request(&node, &outsider_device, "cap-x", NOW))
        .await
        .expect("outsider registers");
    let group = group_text(51);
    let creation = group_creation(
        &group,
        &[(&owner_device, OWNER_ROLE), (&member_device, MEMBER_ROLE)],
    );
    owner.create_group(&creation).await.expect("group created");
    let candidate = application_candidate(&group, &owner_device, 91, b"attachment message");
    let receipt = owner.append_entry(&candidate).await.expect("entry accepted");
    Flow {
        node,
        owner,
        owner_device,
        member,
        outsider,
        group,
        source_seq: receipt.seq,
    }
}

async fn upload(
    flow: &Flow,
    blob_seed: u8,
    chunks: &[&[u8]],
    op_seed: u8,
) -> (String, chat_node_services::blob::BlobReceipt) {
    let total: u64 = chunks.iter().map(|chunk| chunk.len() as u64).sum();
    let chunk_bytes = u32::try_from(chunks[0].len()).expect("chunk size");
    let chunk_count = u32::try_from(chunks.len()).expect("chunk count");
    let descriptor = descriptor(
        blob_seed,
        &flow.group,
        flow.source_seq,
        total,
        chunk_bytes,
        chunk_count,
    );
    let session = flow
        .owner
        .begin_blob_upload(&operation_text(op_seed), &descriptor)
        .await
        .expect("upload begins");
    for (index, chunk) in chunks.iter().enumerate() {
        flow.owner
            .put_blob_chunk(
                &session.upload_id,
                u32::try_from(index).expect("index"),
                chunk,
            )
            .await
            .expect("chunk accepted");
    }
    let manifest = CipherManifest {
        chunk_count,
        ciphertext_bytes: total,
        ciphertext_digest: ciphertext_digest(chunks),
    };
    let receipt = flow
        .owner
        .complete_blob_upload(&session.upload_id, &manifest)
        .await
        .expect("blob published");
    (session.upload_id, receipt)
}

#[tokio::test]
async fn blob_upload_and_read_round_trip_over_http() {
    let flow = setup(NodeAuthPolicy::default()).await;
    let first: &[u8] = b"ciphertext-block-one";
    let second: &[u8] = b"ciphertext-block-two";
    let (upload_id, receipt) = upload(&flow, 121, &[first, second], 221).await;
    assert_eq!(receipt.upload_id, upload_id);
    assert_eq!(receipt.chunk_count, 2);
    assert_eq!(
        receipt.ciphertext_digest,
        ciphertext_digest(&[first, second])
    );
    receipt
        .verify(&flow.node.node_public_key)
        .expect("node-signed publish receipt");
    assert_eq!(receipt.owner_device_id, flow.owner_device.device_id);

    // 上传者与“附件存在时已在群内”的成员都能按块取回完全一致的密文。
    for reader in [&flow.owner, &flow.member] {
        for (index, expected) in [first, second].into_iter().enumerate() {
            let bytes = reader
                .read_blob_chunk(&receipt.blob_id, u32::try_from(index).expect("index"))
                .await
                .expect("chunk read");
            assert_eq!(bytes, expected);
        }
    }

    // 群外设备（已登记但不在群内）不能读。
    let denied = flow
        .outsider
        .read_blob_chunk(&receipt.blob_id, 0)
        .await
        .expect_err("outsider cannot read");
    assert_eq!(denied.kind(), NodeClientErrorKind::Permission);
    assert_eq!(denied.node_code(), Some("HISTORY_NOT_AUTHORIZED"));
    assert_eq!(denied.http_status(), Some(403));
}

#[tokio::test]
async fn duplicate_chunk_is_idempotent_and_size_violation_is_rejected() {
    let flow = setup(NodeAuthPolicy::default()).await;
    let chunk: &[u8] = b"block";
    let descriptor = descriptor(122, &flow.group, flow.source_seq, 10, 5, 2);
    let session = flow
        .owner
        .begin_blob_upload(&operation_text(222), &descriptor)
        .await
        .expect("begin");
    let first = flow
        .owner
        .put_blob_chunk(&session.upload_id, 0, chunk)
        .await
        .expect("chunk accepted");
    let repeat = flow
        .owner
        .put_blob_chunk(&session.upload_id, 0, chunk)
        .await
        .expect("duplicate chunk is idempotent");
    assert_eq!(first, repeat);
    // 超过声明块大小：节点按内容上限拒绝（413 CONTENT_TOO_LARGE）。
    let too_large = flow
        .owner
        .put_blob_chunk(&session.upload_id, 1, b"0123456789")
        .await
        .expect_err("oversized chunk rejected");
    assert_eq!(too_large.node_code(), Some("CONTENT_TOO_LARGE"));
    assert_eq!(too_large.kind(), NodeClientErrorKind::Limit);
    assert_eq!(too_large.http_status(), Some(413));
    // 与声明边界不符（更短）：清单不一致（409）。
    let mismatch = flow
        .owner
        .put_blob_chunk(&session.upload_id, 1, b"012")
        .await
        .expect_err("short chunk rejected");
    assert_eq!(mismatch.node_code(), Some("MANIFEST_MISMATCH"));
    assert_eq!(mismatch.kind(), NodeClientErrorKind::Conflict);
}

#[tokio::test]
async fn out_of_order_and_missing_chunks_are_rejected() {
    let flow = setup(NodeAuthPolicy::default()).await;
    let descriptor = descriptor(123, &flow.group, flow.source_seq, 8, 4, 2);
    let session = flow
        .owner
        .begin_blob_upload(&operation_text(223), &descriptor)
        .await
        .expect("begin");
    let out_of_order = flow
        .owner
        .put_blob_chunk(&session.upload_id, 1, b"tail")
        .await
        .expect_err("out of order rejected");
    assert_eq!(out_of_order.node_code(), Some("CHUNK_OUT_OF_ORDER"));
    assert_eq!(out_of_order.http_status(), Some(409));
    flow.owner
        .put_blob_chunk(&session.upload_id, 0, b"head")
        .await
        .expect("first chunk");
    // 缺末块直接 complete：409 MISSING_CHUNKS。
    let manifest = CipherManifest {
        chunk_count: 2,
        ciphertext_bytes: 8,
        ciphertext_digest: ciphertext_digest(&[b"head", b"tail"]),
    };
    let missing = flow
        .owner
        .complete_blob_upload(&session.upload_id, &manifest)
        .await
        .expect_err("missing chunk rejected");
    assert_eq!(missing.node_code(), Some("MISSING_CHUNKS"));
    assert_eq!(missing.kind(), NodeClientErrorKind::Conflict);
}

#[tokio::test]
async fn digest_mismatch_is_rejected_and_unfinished_blob_is_not_readable() {
    let flow = setup(NodeAuthPolicy::default()).await;
    let first: &[u8] = b"head";
    let second: &[u8] = b"tail";
    let descriptor = descriptor(124, &flow.group, flow.source_seq, 8, 4, 2);
    let session = flow
        .owner
        .begin_blob_upload(&operation_text(224), &descriptor)
        .await
        .expect("begin");
    flow.owner
        .put_blob_chunk(&session.upload_id, 0, first)
        .await
        .expect("chunk 0");
    flow.owner
        .put_blob_chunk(&session.upload_id, 1, second)
        .await
        .expect("chunk 1");
    // 未发布：读不到（404 BLOB_NOT_FOUND），不是空块也不是错误内容。
    let unpublished = flow
        .owner
        .read_blob_chunk(&descriptor.blob_id, 0)
        .await
        .expect_err("unpublished blob is not readable");
    assert_eq!(unpublished.node_code(), Some("BLOB_NOT_FOUND"));
    assert_eq!(unpublished.http_status(), Some(404));
    // 整体摘要不符：409 MANIFEST_MISMATCH，且不会发布。
    let mut manifest = CipherManifest {
        chunk_count: 2,
        ciphertext_bytes: 8,
        ciphertext_digest: ciphertext_digest(&[first, second]),
    };
    manifest.ciphertext_digest = "00".repeat(32);
    let mismatch = flow
        .owner
        .complete_blob_upload(&session.upload_id, &manifest)
        .await
        .expect_err("digest mismatch rejected");
    assert_eq!(mismatch.node_code(), Some("MANIFEST_MISMATCH"));
    assert_eq!(mismatch.http_status(), Some(409));
    let still = flow
        .owner
        .read_blob_chunk(&descriptor.blob_id, 0)
        .await
        .expect_err("still unpublished");
    assert_eq!(still.node_code(), Some("BLOB_NOT_FOUND"));
    // 正确清单可以发布，并可以读回。
    manifest.ciphertext_digest = ciphertext_digest(&[first, second]);
    flow.owner
        .complete_blob_upload(&session.upload_id, &manifest)
        .await
        .expect("published");
    assert_eq!(
        flow.owner
            .read_blob_chunk(&descriptor.blob_id, 1)
            .await
            .expect("read back"),
        second
    );
}

#[tokio::test]
async fn foreign_writer_and_non_canonical_ids_are_rejected() {
    let flow = setup(NodeAuthPolicy::default()).await;
    let descriptor = descriptor(125, &flow.group, flow.source_seq, 8, 4, 2);
    let session = flow
        .owner
        .begin_blob_upload(&operation_text(225), &descriptor)
        .await
        .expect("begin");
    // 非上传设备写块：403。
    let foreign = flow
        .member
        .put_blob_chunk(&session.upload_id, 0, b"head")
        .await
        .expect_err("foreign writer rejected");
    assert_eq!(foreign.kind(), NodeClientErrorKind::Permission);
    assert_eq!(foreign.node_code(), Some("PERMISSION_DENIED"));
    // 非规范 blob 标识在本地就被拒绝，不发请求。
    let bad = flow
        .owner
        .read_blob_chunk("blob-1", 0)
        .await
        .expect_err("non canonical blob id rejected locally");
    assert_eq!(bad.kind(), NodeClientErrorKind::InvalidRequest);
    assert!(bad.http_status().is_none());
    let _ = device_text(21);
    let _ = ErrorCode::InvalidTarget;
}
/// 客户端冻结的 256 KiB 明文分块（密文块 262_173 字节）在**默认节点策略**下能端到端
/// 上传/发布/取回：这是 NODE 分块上限对齐后的回归证据（旧上限 32 KiB/60 KiB 会被拒）。
#[tokio::test]
async fn client_scale_multi_chunk_attachment_round_trips_over_http() {
    use chat_node_services::limits::{
        CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES, MAX_BLOB_CHUNK_BODY_BYTES, MAX_HTTP_BODY_BYTES,
    };

    // 客户端的两个正文上限必须分开：块正文可达分块硬上限，JSON DTO 仍是小上限。
    let limits = chat_node_client::ClientLimits::default();
    assert_eq!(limits.max_chunk_bytes, MAX_BLOB_CHUNK_BODY_BYTES);
    assert_eq!(limits.max_request_bytes, MAX_HTTP_BODY_BYTES);
    assert!(limits.max_chunk_bytes > limits.max_request_bytes);
    assert_eq!(CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES, 256 * 1024 + 29);

    let flow = setup(NodeAuthPolicy::default()).await;
    let full = vec![0xa5_u8; CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES];
    let tail = vec![0x5a_u8; 1_000];
    let (upload_id, receipt) = upload(&flow, 141, &[&full, &full, &tail], 241).await;
    assert_eq!(receipt.upload_id, upload_id);
    assert_eq!(receipt.chunk_count, 3);
    assert_eq!(
        receipt.ciphertext_bytes,
        (CLIENT_ATTACHMENT_CIPHERTEXT_CHUNK_BYTES as u64) * 2 + 1_000
    );
    receipt
        .verify(&flow.node.node_public_key)
        .expect("node-signed publish receipt");

    // 上传者与群成员都能按块取回完全一致的 256 KiB 密文块。
    for reader in [&flow.owner, &flow.member] {
        let bytes = reader
            .read_blob_chunk(&receipt.blob_id, 0)
            .await
            .expect("full chunk read");
        assert_eq!(bytes, full);
        let tail_bytes = reader
            .read_blob_chunk(&receipt.blob_id, 2)
            .await
            .expect("tail chunk read");
        assert_eq!(tail_bytes, tail);
    }
}
