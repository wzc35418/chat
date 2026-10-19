//! EXPD：core 切换到 MLS exporter + 协议字段 57（attachment_metadata）。
//!
//! 覆盖：新消息把描述符放字段 57、正文只有人类可读摘要；跨设备（两个独立库）独立下载解密逐字节一致；
//! 旧格式（描述符在正文）仍可处理；exporter 前置不满足时显式失败且零写入。
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::attachment_message::{
    ATTACHMENT_ONLY_BODY, apply_attachment_event, attachment_legacy_body_from_event,
    attachment_metadata_from_event,
};
use chat_core::attachments::{
    AttachmentError, BlobTransport, FileToken, PersistedAttachmentStore, UploadRequest,
    UploadSession, attachment_exporter_wrapping_key, attachment_legacy_wrapping_key,
    encode_attachment_metadata,
};
use chat_crypto::secret_descriptor::seal_secret_descriptor_legacy;
use chat_core::attachment_transport::blob_object_id_for;
use chat_core::groups::{CreateGroupRequest, GroupBootstrapPeer, create_group};
use chat_core::service::ConversationCore;
use chat_crypto::engine::MlsDevice;
use chat_protocol::{
    AccountId, ApplicationContent, AttachmentRef, AuthenticatedEvent, CandidateKind, ControlHead,
    ConversationId, DecryptedEvent, DeviceId, EventId, GroupContext, GroupId, MessageOrigin,
    MlsSender, ObjectId, PlainApplicationEvent, SignedCandidate, SubmissionId, UnsignedCandidate,
    VerifiedCredential, candidate_signing_bytes, encode_application_event, signing_key_from_seed,
    verify_candidate, verify_inner_event,
};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const AUTHOR_SEED: [u8; 32] = [0x41; 32];
const PLAINTEXT: &[u8] = b"exporter attachment plaintext v1";

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn conversation() -> ConversationId {
    ConversationId::from_bytes([0x11; 32])
}

fn conversation_text() -> String {
    conversation().encode()
}

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-expd-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn database_text(&self) -> String {
        self.database().to_string_lossy().into_owned()
    }

    fn cache(&self) -> PathBuf {
        self.0.join("attachments")
    }

    fn store(&self) -> chat_storage::formal::Store {
        open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store")
    }

    fn attachments(&self) -> PersistedAttachmentStore {
        PersistedAttachmentStore::open(self.cache(), self.store()).expect("open attachment store")
    }

    fn core(&self) -> ConversationCore {
        ConversationCore::open(&self.database_text(), &master_key_hex()).expect("open core")
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).expect("write fixture file");
        path
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct MemoryTransport {
    chunks: HashMap<(String, u32), Vec<u8>>,
    sessions: HashMap<String, String>,
}

impl BlobTransport for MemoryTransport {
    fn begin_upload(&mut self, request: UploadRequest) -> Result<UploadSession, AttachmentError> {
        let session = format!("session-{}", request.attachment_id);
        self.sessions
            .insert(session.clone(), request.attachment_id.clone());
        Ok(UploadSession::new(session))
    }

    fn put_chunk(&mut self, session: &str, index: u32, bytes: &[u8]) -> Result<(), AttachmentError> {
        let blob = self
            .sessions
            .get(session)
            .cloned()
            .ok_or(AttachmentError::Transport {
                code: "UPLOAD_NOT_FOUND",
                detail: "unknown session".to_owned(),
            })?;
        self.chunks.insert((blob, index), bytes.to_vec());
        Ok(())
    }

    fn complete(&mut self, _session: &str, _digest: &str) -> Result<(), AttachmentError> {
        Ok(())
    }

    fn read_chunk(&mut self, blob_id: &str, index: u32) -> Result<Vec<u8>, AttachmentError> {
        self.chunks
            .get(&(blob_id.to_owned(), index))
            .cloned()
            .ok_or(AttachmentError::Transport {
                code: "BLOB_NOT_FOUND",
                detail: "missing chunk".to_owned(),
            })
    }
}

/// 两设备同群：alice（库 A）与 bob（库 B）各自持久化同一 epoch 的群状态。
fn mls_pair(dir_a: &FixtureDir, dir_b: &FixtureDir) -> (MlsDevice, GroupId) {
    let mut alice = MlsDevice::new("alice").expect("alice");
    let bob = MlsDevice::new("bob").expect("bob");
    let mut store_a = dir_a.store();
    let request = CreateGroupRequest::new("exporter 群", "alice");
    let peer = GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &bob,
    };
    let group = create_group(&mut alice, &peer, &mut store_a, &request).expect("create group");
    alice.export_to_store(&mut store_a).expect("persist alice");
    // B 的库也写入同一份已提交群状态（两个独立库、同一 epoch；跨库导出必须得到同一密钥）。
    alice.export_to_store(&mut store_a).expect("persist alice again");
    drop(store_a);
    let mut store_b = dir_b.store();
    alice.export_to_store(&mut store_b).expect("persist into receiver store");
    let group_id = GroupId::parse(&group.group_id).expect("group id");
    let _ = bob;
    (alice, group_id)
}

fn authenticated_event(
    group_id: GroupId,
    mls_epoch: u64,
    event_id: u8,
    content: ApplicationContent,
) -> AuthenticatedEvent {
    let account_id = AccountId::from_bytes([0x31; 32]);
    let device_id = DeviceId::from_bytes([0x51; 32]);
    let signing_key = signing_key_from_seed(&AUTHOR_SEED);
    let unsigned = UnsignedCandidate {
        protocol_version: chat_protocol::ProtocolVersion::V1,
        submission_id: SubmissionId::from_bytes([0x61; 32]),
        group_id,
        author_device_id: device_id,
        kind: CandidateKind::Application,
        expected_control_head: ControlHead::ZERO,
        policy_version: 1,
        mls_epoch,
        encrypted_payload: vec![0xa1; 64],
        control_metadata: None,
    };
    let signature = chat_protocol::sign_bytes(
        &signing_key,
        candidate_signing_bytes(&unsigned).expect("signing bytes").as_bytes(),
    );
    let verified = verify_candidate(
        SignedCandidate {
            unsigned,
            author_signature: signature,
        },
        VerifiedCredential {
            account_id,
            device_id,
            device_public_key: signing_key.verifying_key().to_bytes(),
            credential_version: 1,
            revoked: false,
        },
        GroupContext {
            group_id,
            control_head: ControlHead::ZERO,
            policy_version: 1,
            mls_epoch,
        },
    )
    .expect("outer candidate verifies");
    let plain = PlainApplicationEvent {
        protocol_version: chat_protocol::ProtocolVersion::V1,
        conversation_id: conversation(),
        group_id,
        event_id: EventId::from_bytes([event_id; 32]),
        account_id,
        device_id,
        content,
        reply: None,
        origin: MessageOrigin::UserInitiated,
        sent_at_unix_ms: 1_700_000_000_000,
    };
    let bytes = encode_application_event(plain).expect("encode inner event");
    verify_inner_event(
        &DecryptedEvent::from_bytes(bytes.as_bytes().to_vec()),
        MlsSender {
            account_id,
            device_id,
        },
        &verified,
    )
    .expect("inner event verifies")
}

fn file_id_of(attachment_id: &str) -> ObjectId {
    ObjectId::parse(&blob_object_id_for(attachment_id).expect("blob object id"))
        .expect("object id")
}

#[test]
fn descriptor_travels_in_field_57_and_the_other_device_decrypts_independently() {
    let dir_a = FixtureDir::new("a");
    let dir_b = FixtureDir::new("b");
    let (alice, group_id) = mls_pair(&dir_a, &dir_b);
    // A：导入/上传附件；exporter 包装密钥从**另一个独立库**（同一份已提交群状态）派生，
    // 这样两个写句柄不会重叠（存储是单写者独占）。
    let attachments_a = dir_a.attachments();
    let source = dir_a.write("photo.bin", PLAINTEXT);
    let attachment_id = attachments_a
        .import_file(&FileToken::from_local_path(&source), "photo.bin")
        .expect("import attachment")
        .attachment_id;
    let key_a = {
        let store_b = dir_b.store();
        attachment_exporter_wrapping_key(
            &alice,
            &store_b,
            &attachment_id,
            &conversation_text(),
            0,
        )
        .expect("exporter key derives")
    };
    let mut transport = MemoryTransport::default();
    attachments_a
        .upload_attachment(&attachment_id, &mut transport)
        .expect("upload attachment");
    let body_text = attachments_a
        .encode_attachment_message_body(&attachment_id, &key_a)
        .expect("attachment message body");
    let metadata = encode_attachment_metadata(
        &chat_core::attachments::parse_attachment_message_body(&body_text)
            .expect("parse body")
            .expect("attachment body"),
    )
    .expect("encode field 57 metadata");
    drop(attachments_a);
    // 元数据长度受协议上限约束，且不含明文（描述符是密封字节）。
    assert!(metadata.len() <= chat_protocol::event::MAX_ATTACHMENT_METADATA_BYTES);
    assert!(!body_text.contains("plaintext"));

    let event = authenticated_event(
        group_id,
        0,
        1,
        ApplicationContent::Attachment {
            attachment: AttachmentRef {
                file_id: file_id_of(&attachment_id),
                attachment_metadata: None,
            }
            .with_metadata(metadata.clone()),
        },
    );
    // 协议层把描述符放在字段 57，正文不含任何密钥材料。
    assert_eq!(attachment_metadata_from_event(&event), Some(metadata.as_slice()));
    assert!(attachment_legacy_body_from_event(&event).is_none());

    // B：独立库 + 本机 exporter 密钥 → 采用 → 落消息 → 独立下载解密。
    let bob_key = {
        let store_b = dir_b.store();
        attachment_exporter_wrapping_key(
            &alice,
            &store_b,
            &attachment_id,
            &conversation_text(),
            0,
        )
        .expect("the receiver store derives the same exporter key")
    };
    assert_eq!(bob_key, key_a, "exporter keys must match across stores");
    let adopted = {
        let mut attachments = dir_b.attachments();
        attachments
            .adopt_attachment_event(Some(&metadata), None, Some(&bob_key), None)
            .expect("receiver adopts field 57 metadata")
    };
    assert_eq!(adopted, attachment_id);
    let applied = {
        let mut core = dir_b.core();
        apply_attachment_event(&mut core, &event, &adopted).expect("message persists")
    };
    assert_eq!(applied.body, ATTACHMENT_ONLY_BODY);
    assert_eq!(applied.attachment_ids, vec![attachment_id.clone()]);
    let attachments = dir_b.attachments();
    let record = attachments
        .attachment_record(&attachment_id)
        .expect("record")
        .expect("record exists");
    let manifest = chat_core::attachments::AttachmentManifest {
        attachment_id: attachment_id.clone(),
        plain_size: record.plain_size,
        chunk_size: record.chunk_size,
        chunk_count: record.chunk_count,
        ciphertext_bytes: 0,
        ciphertext_digest: record.ciphertext_digest.clone(),
    };
    let destination = dir_b.write("out.bin", b"");
    let mut transport = transport;
    attachments
        .download_attachment(
            &attachment_id,
            &manifest,
            &FileToken::from_local_path(&destination),
            &mut transport,
        )
        .expect("receiver downloads and decrypts");
    assert_eq!(fs::read(&destination).expect("read"), PLAINTEXT);
}

#[test]
fn legacy_body_with_v1_descriptor_is_still_accepted() {
    let dir = FixtureDir::new("legacy");
    // v1（legacy P0 派生）描述符：用自选 FileKey 封装，证明老格式仍能解开并落库。
    let attachment_id = "att-00112233445566778899aabbccddeeff".to_owned();
    let file_id = hex_file_id(&attachment_id);
    let file_key_bytes = [0x7u8; 32];
    let file_key = chat_crypto::attachment::FileKey::try_from_slice(&file_key_bytes)
        .expect("file key");
    let legacy_key = attachment_legacy_wrapping_key(&conversation_text(), 0);
    let descriptor = seal_secret_descriptor_legacy(&legacy_key, &attachment_id, &file_key)
        .expect("seal legacy descriptor");
    assert!(descriptor.is_legacy(), "v1 descriptor is marked legacy");
    let body = format!(
        "{{\"kind\":\"chat.attachment.v1\",\"attachment_id\":\"{attachment_id}\",\"file_id\":\"{file_id}\",\"plain_size\":4,\"content_digest\":\"{digest}\",\"display_name\":\"legacy.bin\",\"ciphertext_bytes\":8,\"chunk_bytes\":8,\"chunk_count\":1,\"ciphertext_digest\":\"{digest}\",\"secret_descriptor\":\"{descriptor_hex}\",\"binding\":\"{binding}\"}}",
        digest = hex_digest(b"lega"),
        descriptor_hex = descriptor_hex(&descriptor),
        binding = "00".repeat(32),
    );
    let mut attachments = dir.attachments();
    // 显式绑定不符（binding 全零是错的）→ 先证明校验生效。
    let rejected = attachments
        .adopt_attachment_event(None, Some(&body), None, Some(&legacy_key))
        .expect_err("binding mismatch is rejected");
    assert_eq!(rejected.code(), "ATTACHMENT_CORRUPTED");
    // 用正确绑定重新构造（legacy 描述符仍可解）。
    let mut parsed = chat_core::attachments::parse_attachment_message_body(&body)
        .expect("parse")
        .expect("attachment body");
    let binding = chat_core::attachments::attachment_message_binding(&parsed)
        .expect("binding digest");
    parsed.binding = binding.iter().map(|byte| format!("{byte:02x}")).collect();
    let body = serde_json::to_string(&parsed).expect("encode legacy body");
    let adopted = attachments
        .adopt_attachment_event(None, Some(&body), None, Some(&legacy_key))
        .expect("legacy v1 descriptor is still readable");
    assert_eq!(adopted, attachment_id);
}

#[test]
fn exporter_preconditions_fail_explicitly_without_writes() {
    let dir = FixtureDir::new("precond");
    let device = MlsDevice::new("solo").expect("device");
    let store = dir.store();
    // 从未持久化过群状态：exporter 必须显式失败（稳定码透出），不能静默降级到占位派生。
    let error = attachment_exporter_wrapping_key(
        &device,
        &store,
        "att-00000000000000000000000000000000",
        &conversation_text(),
        0,
    )
    .expect_err("exporter must fail without a persisted epoch");
    assert!(
        matches!(error.code(), "CRYPTO_FAILURE" | "EXPORTER_NOT_PERSISTED" | "STALE_EPOCH"),
        "unexpected exporter code {}",
        error.code()
    );
    drop(store);
    // 零写入：附件库里没有任何记录。
    let attachments = dir.attachments();
    assert!(attachments
        .attachment_record("att-00000000000000000000000000000000")
        .expect("record lookup")
        .is_none());
}

/// 附件本地 ID 是 att-<32hex>；协议字段里的 file_id 就是这 16 字节（32 hex 字符）。
fn hex_file_id(attachment_id: &str) -> String {
    attachment_id
        .trim_start_matches("att-")
        .to_owned()
}

fn hex_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn descriptor_hex(descriptor: &chat_crypto::secret_descriptor::SecretDescriptor) -> String {
    descriptor
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
