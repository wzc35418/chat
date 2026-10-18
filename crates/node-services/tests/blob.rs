//! NODE-012~015 附件密文 blob 的服务层测试。
//!
//! 全部走公开 API 与真实 SQLite：正常往返、重复块幂等、缺块/乱序拒绝、摘要不符拒绝、
//! 越权/未发布拒绝、尺寸与块数上限、配额、会话过期、operation 幂等与成员窗口。
mod support;

use std::path::PathBuf;

use chat_node_services::blob::{
    BlobReceipt, CipherManifest, PublicBlobDescriptor, UploadSession,
};
use chat_node_services::canonical::hex_encode;
use chat_node_services::limits::BlobPolicy;
use chat_node_services::services::{AuthenticatedDevice, NodeAuthPolicy};
use chat_protocol::{ObjectId, OperationId};
use sha2::{Digest, Sha256};
use support::{Harness, NOW, TestDevice, group_text, submission_text};

const OWNER_ROLE: &str = "owner";
const MEMBER_ROLE: &str = "member";

fn object_text(seed: u8) -> String {
    ObjectId::from_bytes([seed; 32]).encode()
}

fn operation_text(seed: u8) -> String {
    OperationId::from_bytes([seed; 32]).encode()
}

/// 整体密文摘要 = 按块序拼接后的一次 SHA-256（与节点侧校验一致）。
fn ciphertext_digest(chunks: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for chunk in chunks {
        hasher.update(chunk);
    }
    hex_encode(&hasher.finalize())
}

fn descriptor(
    blob_seed: u8,
    group_id: &str,
    source_seq: u64,
    ciphertext_bytes: u64,
    chunk_bytes: u32,
    chunk_count: u32,
) -> PublicBlobDescriptor {
    PublicBlobDescriptor {
        blob_id: object_text(blob_seed),
        group_id: group_id.to_owned(),
        source_seq,
        ciphertext_bytes,
        chunk_bytes,
        chunk_count,
    }
}

/// 两成员群 + 一条已接受日志条目（作者=owner 设备）。
struct Fixture {
    harness: Harness,
    member: TestDevice,
    owner_actor: AuthenticatedDevice,
    member_actor: AuthenticatedDevice,
    group: String,
    source_seq: u64,
    dir: Option<PathBuf>,
}

impl Fixture {
    fn in_memory() -> Self {
        Self::build(Harness::in_memory(), None)
    }

    fn in_memory_with(policy: NodeAuthPolicy) -> Self {
        Self::build(Harness::in_memory_with(policy), None)
    }

    fn file(tag: &str, policy: NodeAuthPolicy) -> Self {
        let dir = std::env::temp_dir().join(format!("chat-node-blob-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let harness = Harness::open(&dir.join("node.db"), policy);
        Self::build(harness, Some(dir))
    }

    fn build(mut harness: Harness, dir: Option<PathBuf>) -> Self {
        let owner = Harness::device(21);
        let member = Harness::device(22);
        let owner_actor = harness.registered_actor(&owner, NOW);
        harness.register(&member, NOW).expect("member registers");
        let group = group_text(41);
        harness
            .service
            .register_group(
                &harness.signed_creation(
                    &owner,
                    &group,
                    &[(21, OWNER_ROLE), (22, MEMBER_ROLE)],
                ),
                &owner_actor,
                NOW,
            )
            .expect("group registers");
        let candidate = harness.application_candidate(&owner, &group, 77, b"attachment message");
        let receipt = harness
            .service
            .append_group_entry(&group, &candidate, &owner_actor, NOW)
            .expect("entry accepted");
        let member_actor = harness.authenticate(&member, NOW).expect("member auth");
        Self {
            harness,
            member,
            owner_actor,
            member_actor,
            group,
            source_seq: receipt.seq,
            dir,
        }
    }

    /// 上传一个完整的 blob，返回 (upload id, 发布回执)。
    fn upload(&mut self, blob_seed: u8, chunks: &[&[u8]], op_seed: u8) -> (String, BlobReceipt) {
        let total: u64 = chunks.iter().map(|chunk| chunk.len() as u64).sum();
        let chunk_bytes = u32::try_from(chunks[0].len()).expect("chunk size");
        let chunk_count = u32::try_from(chunks.len()).expect("chunk count");
        let descriptor = descriptor(
            blob_seed,
            &self.group,
            self.source_seq,
            total,
            chunk_bytes,
            chunk_count,
        );
        let session = self
            .harness
            .service
            .begin_blob_upload(&descriptor, &operation_text(op_seed), &self.owner_actor, NOW)
            .expect("upload begins");
        for (index, chunk) in chunks.iter().enumerate() {
            self.harness
                .service
                .put_blob_chunk(
                    &session.upload_id,
                    u32::try_from(index).expect("index"),
                    chunk,
                    &self.owner_actor,
                    NOW,
                )
                .expect("chunk accepted");
        }
        let manifest = CipherManifest {
            chunk_count,
            ciphertext_bytes: total,
            ciphertext_digest: ciphertext_digest(chunks),
        };
        let receipt = self
            .harness
            .service
            .complete_blob_upload(&session.upload_id, &manifest, &self.owner_actor, NOW)
            .expect("blob published");
        (session.upload_id, receipt)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn small_blob_policy() -> NodeAuthPolicy {
    NodeAuthPolicy {
        blob: BlobPolicy {
            max_bytes_per_device: 64,
            max_object_bytes: 32,
            max_chunk_bytes: 8,
            max_chunk_count: 4,
            upload_ttl_seconds: 60,
        },
        ..NodeAuthPolicy::default()
    }
}

#[test]
fn upload_round_trip_returns_exact_ciphertext_chunks() {
    let mut fixture = Fixture::in_memory();
    let first: &[u8] = b"ciphertext";
    let second: &[u8] = b"tail!!";
    let (upload_id, receipt) = fixture.upload(101, &[first, second], 201);
    assert_eq!(receipt.upload_id, upload_id);
    assert_eq!(receipt.chunk_count, 2);
    assert_eq!(receipt.ciphertext_bytes, 16);
    assert_eq!(receipt.ciphertext_digest, ciphertext_digest(&[first, second]));
    receipt
        .verify(&fixture.harness.node_public_key())
        .expect("node-signed publish receipt");

    // 上传者本人可以按块取回完全一致的密文。
    for (index, expected) in [first, second].into_iter().enumerate() {
        let chunk: chat_node_services::blob::CipherChunk = fixture
            .harness
            .service
            .read_blob_chunk(
                &receipt.blob_id,
                u32::try_from(index).expect("index"),
                &fixture.owner_actor,
                NOW,
            )
            .expect("owner reads chunk");
        assert_eq!(chunk.bytes, expected);
        assert_eq!(chunk.chunk_count, 2);
        assert_eq!(chunk.cipher_hash, hex_encode(&Sha256::digest(expected)));
    }

    // 附件存在时就已在群内的成员也能读。
    let chunk = fixture
        .harness
        .service
        .read_blob_chunk(&receipt.blob_id, 0, &fixture.member_actor, NOW)
        .expect("member reads chunk");
    assert_eq!(chunk.bytes, first);
}

#[test]
fn duplicate_chunk_is_idempotent_and_divergent_chunk_is_rejected() {
    let mut fixture = Fixture::in_memory();
    let chunk = b"chunk-one0";
    let descriptor = descriptor(102, &fixture.group, fixture.source_seq, 10, 10, 1);
    let session: UploadSession = fixture
        .harness
        .service
        .begin_blob_upload(&descriptor, &operation_text(202), &fixture.owner_actor, NOW)
        .expect("begin");
    let first = fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, chunk, &fixture.owner_actor, NOW)
        .expect("chunk accepted");
    assert_eq!(first.received_chunks, 1);
    // 同块同字节重传：同一份回执，不占第二份。
    let repeat = fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, chunk, &fixture.owner_actor, NOW)
        .expect("repeat accepted");
    assert_eq!(first, repeat);
    // 同序号换密文：CHUNK_CONFLICT。
    let conflict = fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, b"other-one", &fixture.owner_actor, NOW)
        .expect_err("divergent chunk rejected");
    assert_eq!(conflict.code(), "CHUNK_CONFLICT");
}

#[test]
fn chunks_must_arrive_in_order_and_index_is_bounded() {
    let mut fixture = Fixture::in_memory();
    let descriptor = descriptor(103, &fixture.group, fixture.source_seq, 8, 4, 2);
    let session = fixture
        .harness
        .service
        .begin_blob_upload(&descriptor, &operation_text(203), &fixture.owner_actor, NOW)
        .expect("begin");
    // 乱序：先写第 1 块。
    let out_of_order = fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 1, b"tail", &fixture.owner_actor, NOW)
        .expect_err("out of order rejected");
    assert_eq!(out_of_order.code(), "CHUNK_OUT_OF_ORDER");
    // 越界序号。
    let out_of_range = fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 9, b"x", &fixture.owner_actor, NOW)
        .expect_err("index out of range");
    assert_eq!(out_of_range.code(), "INVALID_REQUEST");
    // 空块。
    let empty = fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, b"", &fixture.owner_actor, NOW)
        .expect_err("empty chunk rejected");
    assert_eq!(empty.code(), "INVALID_REQUEST");
    // 按序之后成功。
    fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, b"head", &fixture.owner_actor, NOW)
        .expect("in order accepted");
    fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 1, b"tail", &fixture.owner_actor, NOW)
        .expect("second chunk accepted");
}

#[test]
fn complete_rejects_missing_chunks_and_manifest_mismatch() {
    let mut fixture = Fixture::in_memory();
    let chunks: [&[u8]; 2] = [b"head", b"tail"];
    let descriptor = descriptor(104, &fixture.group, fixture.source_seq, 8, 4, 2);
    let session = fixture
        .harness
        .service
        .begin_blob_upload(&descriptor, &operation_text(204), &fixture.owner_actor, NOW)
        .expect("begin");
    fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, chunks[0], &fixture.owner_actor, NOW)
        .expect("first chunk");
    let manifest = CipherManifest {
        chunk_count: 2,
        ciphertext_bytes: 8,
        ciphertext_digest: ciphertext_digest(&chunks),
    };
    // 缺末块。
    let missing = fixture
        .harness
        .service
        .complete_blob_upload(&session.upload_id, &manifest, &fixture.owner_actor, NOW)
        .expect_err("missing chunk rejected");
    assert_eq!(missing.code(), "MISSING_CHUNKS");
    fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 1, chunks[1], &fixture.owner_actor, NOW)
        .expect("second chunk");
    // 整体摘要不符。
    let mut bad_digest = manifest.clone();
    bad_digest.ciphertext_digest = "00".repeat(32);
    let wrong = fixture
        .harness
        .service
        .complete_blob_upload(&session.upload_id, &bad_digest, &fixture.owner_actor, NOW)
        .expect_err("digest mismatch rejected");
    assert_eq!(wrong.code(), "MANIFEST_MISMATCH");
    // 块数不符。
    let mut wrong_count = manifest.clone();
    wrong_count.chunk_count = 3;
    let wrong = fixture
        .harness
        .service
        .complete_blob_upload(&session.upload_id, &wrong_count, &fixture.owner_actor, NOW)
        .expect_err("chunk count mismatch rejected");
    assert_eq!(wrong.code(), "MANIFEST_MISMATCH");
    // 总长不符。
    let mut wrong_bytes = manifest.clone();
    wrong_bytes.ciphertext_bytes = 9;
    let wrong = fixture
        .harness
        .service
        .complete_blob_upload(&session.upload_id, &wrong_bytes, &fixture.owner_actor, NOW)
        .expect_err("byte length mismatch rejected");
    assert_eq!(wrong.code(), "MANIFEST_MISMATCH");
    // 正确清单可以发布。
    fixture
        .harness
        .service
        .complete_blob_upload(&session.upload_id, &manifest, &fixture.owner_actor, NOW)
        .expect("published");
}
#[test]
fn complete_is_idempotent_and_conflicting_retry_is_rejected() {
    let mut fixture = Fixture::in_memory();
    let chunk = b"only-chunk";
    let (upload_id, receipt) = fixture.upload(105, &[chunk], 205);
    let manifest = CipherManifest {
        chunk_count: 1,
        ciphertext_bytes: 10,
        ciphertext_digest: ciphertext_digest(&[chunk]),
    };
    // 重复 complete：同一份回执。
    let repeat = fixture
        .harness
        .service
        .complete_blob_upload(&upload_id, &manifest, &fixture.owner_actor, NOW)
        .expect("repeat complete");
    assert_eq!(receipt, repeat);
    // 已完成后再写块：序号已存在且字节相同则幂等，否则冲突。
    let same = fixture
        .harness
        .service
        .put_blob_chunk(&upload_id, 0, chunk, &fixture.owner_actor, NOW)
        .expect("same chunk after complete is idempotent");
    assert_eq!(same.cipher_hash, receipt.ciphertext_digest);
    // 相异清单的重复 complete 显式失败。
    let mut divergent = manifest.clone();
    divergent.ciphertext_digest = "11".repeat(32);
    let error = fixture
        .harness
        .service
        .complete_blob_upload(&upload_id, &divergent, &fixture.owner_actor, NOW)
        .expect_err("divergent manifest rejected");
    assert_eq!(error.code(), "MANIFEST_MISMATCH");
}

#[test]
fn unpublished_blob_is_not_readable_and_owner_is_enforced() {
    let mut fixture = Fixture::in_memory();
    let chunk = b"secret-ct";
    let descriptor = descriptor(106, &fixture.group, fixture.source_seq, 9, 9, 1);
    let session = fixture
        .harness
        .service
        .begin_blob_upload(&descriptor, &operation_text(206), &fixture.owner_actor, NOW)
        .expect("begin");
    // 未发布（甚至未上传）时不可读。
    let not_published = fixture
        .harness
        .service
        .read_blob_chunk(&descriptor.blob_id, 0, &fixture.owner_actor, NOW)
        .expect_err("unpublished blob is not readable");
    assert_eq!(not_published.code(), "BLOB_NOT_FOUND");
    // 只有上传设备本人能写块。
    let foreign = fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, chunk, &fixture.member_actor, NOW)
        .expect_err("foreign writer rejected");
    assert_eq!(foreign.code(), "PERMISSION_DENIED");
    fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, chunk, &fixture.owner_actor, NOW)
        .expect("owner writes chunk");
    // 上传完但未发布：仍不可读。
    let not_published = fixture
        .harness
        .service
        .read_blob_chunk(&descriptor.blob_id, 0, &fixture.member_actor, NOW)
        .expect_err("still unpublished");
    assert_eq!(not_published.code(), "BLOB_NOT_FOUND");
}

#[test]
fn size_and_count_limits_are_enforced() {
    let mut fixture = Fixture::in_memory_with(small_blob_policy());
    // 声明总长超过对象上限。
    let too_big = descriptor(107, &fixture.group, fixture.source_seq, 33, 8, 5);
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&too_big, &operation_text(207), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "CONTENT_TOO_LARGE"
    );
    // 块数超过上限。
    let too_many = descriptor(107, &fixture.group, fixture.source_seq, 32, 8, 5);
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&too_many, &operation_text(207), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "CONTENT_TOO_LARGE"
    );
    // 声明块大小超过上限（8）。
    let too_large_chunk = descriptor(107, &fixture.group, fixture.source_seq, 9, 9, 1);
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&too_large_chunk, &operation_text(207), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "CONTENT_TOO_LARGE"
    );
    // 声明边界不自洽（块数与总长矛盾）。
    let inconsistent = descriptor(107, &fixture.group, fixture.source_seq, 8, 4, 3);
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&inconsistent, &operation_text(207), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    // 实际块超过声明块大小 → CONTENT_TOO_LARGE；小于声明的块边界 → MANIFEST_MISMATCH。
    let valid = descriptor(108, &fixture.group, fixture.source_seq, 8, 4, 2);
    let session = fixture
        .harness
        .service
        .begin_blob_upload(&valid, &operation_text(208), &fixture.owner_actor, NOW)
        .expect("begin");
    assert_eq!(
        fixture
            .harness
            .service
            .put_blob_chunk(&session.upload_id, 0, b"123456789", &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "CONTENT_TOO_LARGE"
    );
    assert_eq!(
        fixture
            .harness
            .service
            .put_blob_chunk(&session.upload_id, 0, b"123", &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "MANIFEST_MISMATCH"
    );
}

#[test]
fn device_quota_is_enforced_across_uploads() {
    let mut fixture = Fixture::in_memory_with(small_blob_policy());
    // 配额 64 字节：先预留 32。
    let first = descriptor(109, &fixture.group, fixture.source_seq, 32, 8, 4);
    fixture
        .harness
        .service
        .begin_blob_upload(&first, &operation_text(209), &fixture.owner_actor, NOW)
        .expect("first upload reserved");
    // 再预留 32 也放得下。
    let second = descriptor(110, &fixture.group, fixture.source_seq, 32, 8, 4);
    fixture
        .harness
        .service
        .begin_blob_upload(&second, &operation_text(210), &fixture.owner_actor, NOW)
        .expect("second upload reserved");
    // 第三份 32 字节会超过 64 字节上限：在传预留也算配额。
    let third = descriptor(111, &fixture.group, fixture.source_seq, 32, 8, 4);
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&third, &operation_text(211), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "QUOTA_EXCEEDED"
    );
}

#[test]
fn operation_dedup_returns_same_session_and_conflicts_on_divergence() {
    let mut fixture = Fixture::in_memory();
    let descriptor_a = descriptor(112, &fixture.group, fixture.source_seq, 8, 4, 2);
    let first = fixture
        .harness
        .service
        .begin_blob_upload(&descriptor_a, &operation_text(212), &fixture.owner_actor, NOW)
        .expect("begin");
    first
        .verify(&fixture.harness.node_public_key())
        .expect("node-signed session");
    let again = fixture
        .harness
        .service
        .begin_blob_upload(&descriptor_a, &operation_text(212), &fixture.owner_actor, NOW)
        .expect("idempotent begin");
    assert_eq!(first, again, "same operation + same descriptor reuses the session");
    // 同 operation 换描述 → 冲突。
    let mut divergent = descriptor_a.clone();
    divergent.blob_id = object_text(113);
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&divergent, &operation_text(212), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "BLOB_UPLOAD_CONFLICT"
    );
    // 同 blob id 换 operation → 冲突。
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&descriptor_a, &operation_text(213), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "BLOB_UPLOAD_CONFLICT"
    );
}

#[test]
fn expired_upload_session_rejects_chunks_and_completion() {
    let mut fixture = Fixture::in_memory_with(NodeAuthPolicy {
        blob: BlobPolicy {
            upload_ttl_seconds: 10,
            ..BlobPolicy::default()
        },
        ..NodeAuthPolicy::default()
    });
    let chunk = b"ciphertext";
    let descriptor = descriptor(114, &fixture.group, fixture.source_seq, 10, 10, 1);
    let session = fixture
        .harness
        .service
        .begin_blob_upload(&descriptor, &operation_text(214), &fixture.owner_actor, NOW)
        .expect("begin");
    let later = NOW + 11;
    let expired = fixture
        .harness
        .service
        .put_blob_chunk(&session.upload_id, 0, chunk, &fixture.owner_actor, later)
        .expect_err("expired session rejects chunks");
    assert_eq!(expired.code(), "UPLOAD_EXPIRED");
    let manifest = CipherManifest {
        chunk_count: 1,
        ciphertext_bytes: 10,
        ciphertext_digest: ciphertext_digest(&[chunk]),
    };
    let expired = fixture
        .harness
        .service
        .complete_blob_upload(&session.upload_id, &manifest, &fixture.owner_actor, later)
        .expect_err("expired session cannot complete");
    assert_eq!(expired.code(), "UPLOAD_EXPIRED");
}

#[test]
fn unknown_identifiers_and_foreign_uploads_are_rejected() {
    let mut fixture = Fixture::in_memory();
    assert_eq!(
        fixture
            .harness
            .service
            .put_blob_chunk(&"upl_missing", 0, b"x", &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "UPLOAD_NOT_FOUND"
    );
    let manifest = CipherManifest {
        chunk_count: 1,
        ciphertext_bytes: 1,
        ciphertext_digest: ciphertext_digest(&[b"x"]),
    };
    assert_eq!(
        fixture
            .harness
            .service
            .complete_blob_upload(&"upl_missing", &manifest, &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "UPLOAD_NOT_FOUND"
    );
    assert_eq!(
        fixture
            .harness
            .service
            .read_blob_chunk(&object_text(115), 0, &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "BLOB_NOT_FOUND"
    );
    // 别的设备不能借同一条日志条目开上传会话（授权先于标识冲突判定）。
    let descriptor = descriptor(116, &fixture.group, fixture.source_seq, 8, 4, 2);
    fixture
        .harness
        .service
        .begin_blob_upload(&descriptor, &operation_text(216), &fixture.owner_actor, NOW)
        .expect("owner begin");
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&descriptor, &operation_text(217), &fixture.member_actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );
}

#[test]
fn source_entry_and_membership_window_gate_uploads_and_reads() {
    let mut fixture = Fixture::file("window", NodeAuthPolicy::default());
    // 关联条目必须存在且作者是上传设备本人。
    let missing = descriptor(117, &fixture.group, fixture.source_seq + 9, 8, 4, 2);
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&missing, &operation_text(218), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );
    // 成员不能借别人的条目上传附件。
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(
                &descriptor(118, &fixture.group, fixture.source_seq, 8, 4, 2),
                &operation_text(219),
                &fixture.member_actor,
                NOW,
            )
            .unwrap_err()
            .code(),
        "PERMISSION_DENIED"
    );

    // 正常发布一份附件。
    let chunks: [&[u8]; 2] = [b"head", b"tail"];
    let (_, receipt) = fixture.upload(119, &chunks, 220);
    let read = |fixture: &mut Fixture| {
        fixture
            .harness
            .service
            .read_blob_chunk(&receipt.blob_id, 0, &fixture.member_actor, NOW)
            .map(|chunk| chunk.bytes)
    };
    assert_eq!(read(&mut fixture).expect("member can read"), chunks[0]);

    // 把成员的加入点挪到附件之后：读旧附件必须被拒绝（新成员拿不到旧密文）。
    let dir = fixture.dir.clone().expect("file fixture");
    {
        let connection = rusqlite::Connection::open(dir.join("node.db")).expect("direct db");
        connection
            .execute(
                "UPDATE group_members SET joined_seq = ?1 WHERE group_id = ?2 AND device_id = ?3",
                rusqlite::params![
                    (fixture.source_seq + 1) as i64,
                    fixture.group.as_str(),
                    fixture.member.device_id.as_str()
                ],
            )
            .expect("shift join window");
    }
    let denied = read(&mut fixture).expect_err("history window denies the old attachment");
    assert_eq!(denied.code(), "HISTORY_NOT_AUTHORIZED");
}

#[test]
fn descriptor_validation_rejects_malformed_shapes() {
    let mut fixture = Fixture::in_memory();
    let invalid_blob_id = PublicBlobDescriptor {
        blob_id: "obj-1".into(),
        ..descriptor(120, &fixture.group, fixture.source_seq, 8, 4, 2)
    };
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&invalid_blob_id, &operation_text(221), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    let mut zero_bytes = descriptor(121, &fixture.group, fixture.source_seq, 8, 4, 2);
    zero_bytes.ciphertext_bytes = 0;
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(&zero_bytes, &operation_text(222), &fixture.owner_actor, NOW)
            .unwrap_err()
            .code(),
        "CONTENT_TOO_LARGE"
    );
    // 非规范 operation 标识。
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(
                &descriptor(122, &fixture.group, fixture.source_seq, 8, 4, 2),
                "op-1",
                &fixture.owner_actor,
                NOW,
            )
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
    // 未登记设备伪造 actor。
    let forged = AuthenticatedDevice {
        account_id: "acct-a".into(),
        device_id: support::device_text(99),
        credential_version: 1,
        challenge_id: "forged".into(),
    };
    assert_eq!(
        fixture
            .harness
            .service
            .begin_blob_upload(
                &descriptor(123, &fixture.group, fixture.source_seq, 8, 4, 2),
                &operation_text(223),
                &forged,
                NOW,
            )
            .unwrap_err()
            .code(),
        "INVALID_CREDENTIAL"
    );
    let _ = submission_text(1);
}
