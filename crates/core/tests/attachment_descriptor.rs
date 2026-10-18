//! SECD：附件密钥描述符随消息投递的端到端用例（真 MLS 双设备 + 两个独立库）。
//!
//! 覆盖：A 导入/上传 → 消息体带描述符 → **B 在独立库里**只凭消息 + 群包装密钥复现附件记录与
//! FileKey，并独立下载解密；描述符/摘要被篡改 → 拒绝且零写入；旧式引用（无描述符）→
//! 行为不变、显式 ATTACHMENT_KEY_UNAVAILABLE。
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::attachment_message::apply_attachment_message_event;
use chat_core::attachment_transport::blob_object_id_for;
use chat_core::attachments::{
    AttachmentError, BlobTransport, FileToken, PersistedAttachmentStore, UploadRequest,
    UploadSession, attachment_message_wrapping_key, parse_attachment_message_body,
};
use chat_core::service::ConversationCore;
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
const PLAINTEXT: &[u8] = b"cross-device attachment plaintext v1";

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
            "chat-core-secd-{tag}-{}-{nanos}-{counter}",
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

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).expect("write fixture file");
        path
    }

    fn attachments(&self) -> PersistedAttachmentStore {
        let store = open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store");
        PersistedAttachmentStore::open(self.cache(), store).expect("open attachment store")
    }

    fn core(&self) -> ConversationCore {
        ConversationCore::open(&self.database_text(), &master_key_hex()).expect("open core")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 内存块传输替身：上传写进共享表，下载按 blob+序号读回。
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

/// 用真实协议层构造一条“已验证应用事件”。
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

fn group() -> GroupId {
    GroupId::from_bytes([0x71; 32])
}

fn wrapping_key() -> [u8; 32] {
    attachment_message_wrapping_key(&conversation_text(), 0)
}

/// A 侧：导入 + 上传，返回 (attachment_id, 消息体文本)。
fn prepare_sender(dir: &FixtureDir) -> (String, String, MemoryTransport) {
    let attachments = dir.attachments();
    let source = dir.write("photo.bin", PLAINTEXT);
    let attachment_id = attachments
        .import_file(&FileToken::from_local_path(&source), "photo.bin")
        .expect("import attachment")
        .attachment_id;
    let mut transport = MemoryTransport::default();
    attachments
        .upload_attachment(&attachment_id, &mut transport)
        .expect("upload attachment");
    let body = attachments
        .encode_attachment_message_body(&attachment_id, &wrapping_key())
        .expect("encode attachment message body");
    (attachment_id, body, transport)
}

#[test]
fn another_store_can_decrypt_the_attachment_with_the_descriptor() {
    let sender_dir = FixtureDir::new("sender");
    let (attachment_id, body, transport) = prepare_sender(&sender_dir);
    let parsed = parse_attachment_message_body(&body)
        .expect("body parses")
        .expect("body is an attachment message");
    assert_eq!(parsed.attachment_id, attachment_id);

    // B：全新的库与缓存目录（从未见过 A 的任何本地数据）。
    let receiver_dir = FixtureDir::new("receiver");
    // 1) 附件库：描述符 → 记录 + 0007 密钥（单写者：用完即释放）。
    let adopted = {
        let mut attachments = receiver_dir.attachments();
        attachments
            .adopt_attachment_message(&parsed, &wrapping_key())
            .expect("receiver adopts the descriptor")
    };
    assert_eq!(adopted, attachment_id);
    // 2) 内核：消息 + 引用落库。
    let event = authenticated_event(
        group(),
        0,
        1,
        ApplicationContent::Text { text: body.clone() },
    );
    let applied = {
        let mut core = receiver_dir.core();
        apply_attachment_message_event(&mut core, &event, &adopted)
            .expect("receiver persists the message and the reference")
    };
    assert_eq!(applied.attachment_ids, vec![attachment_id.clone()]);
    assert_eq!(
        applied.body,
        chat_core::attachment_message::ATTACHMENT_ONLY_BODY
    );
    // 3) 附件库：独立下载 + 解密（文件内容与 A 的明文一致）。
    let attachments = receiver_dir.attachments();
    let record = attachments
        .attachment_record(&attachment_id)
        .expect("record")
        .expect("record exists in B");
    let manifest = chat_core::attachments::AttachmentManifest {
        attachment_id: attachment_id.clone(),
        plain_size: record.plain_size,
        chunk_size: record.chunk_size,
        chunk_count: record.chunk_count,
        ciphertext_bytes: 0,
        ciphertext_digest: record.ciphertext_digest.clone(),
    };
    let destination = receiver_dir.write("out.bin", b"");
    let mut transport = transport;
    attachments
        .download_attachment(
            &attachment_id,
            &manifest,
            &FileToken::from_local_path(&destination),
            &mut transport,
        )
        .expect("receiver downloads and decrypts");
    assert_eq!(
        fs::read(&destination).expect("read downloaded file"),
        PLAINTEXT
    );
}

#[test]
fn tampered_descriptor_or_manifest_is_rejected_without_writes() {
    let sender_dir = FixtureDir::new("tamper-src");
    let (attachment_id, body, _transport) = prepare_sender(&sender_dir);
    let original = parse_attachment_message_body(&body)
        .expect("parse")
        .expect("attachment body");

    let receiver_dir = FixtureDir::new("tamper-dst");
    let mut attachments = receiver_dir.attachments();
    // ① 篡改密文摘要（清单）→ 绑定不符 → ATTACHMENT_CORRUPTED，且零写入。
    let mut tampered = original.clone();
    tampered.ciphertext_digest = format!("00{}", &original.ciphertext_digest[2..]);
    let error = attachments
        .adopt_attachment_message(&tampered, &wrapping_key())
        .expect_err("tampered manifest is rejected");
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    assert!(
        attachments
            .attachment_record(&attachment_id)
            .expect("record lookup")
            .is_none(),
        "no half record may be written"
    );
    // ② 篡改描述符字节 → 打不开 → ATTACHMENT_KEY_UNAVAILABLE，同样零写入。
    let mut tampered = original.clone();
    let replacement = if tampered.secret_descriptor.starts_with("00") {
        "11"
    } else {
        "00"
    };
    tampered.secret_descriptor.replace_range(0..2, replacement);
    let error = attachments
        .adopt_attachment_message(&tampered, &wrapping_key())
        .expect_err("tampered descriptor is rejected");
    assert_eq!(error.code(), "ATTACHMENT_KEY_UNAVAILABLE");
    assert!(attachments
        .attachment_record(&attachment_id)
        .expect("record lookup")
        .is_none());
    // ③ 换一个会话的包装密钥 → 打不开（绑定到会话/epoch）。
    let mut other_key = wrapping_key();
    other_key[0] ^= 0xff;
    let error = attachments
        .adopt_attachment_message(&original, &other_key)
        .expect_err("another conversation key cannot open the descriptor");
    assert_eq!(error.code(), "ATTACHMENT_KEY_UNAVAILABLE");
    assert!(attachments
        .attachment_record(&attachment_id)
        .expect("record lookup")
        .is_none());
}

#[test]
fn legacy_attachment_reference_without_descriptor_cannot_be_downloaded() {
    let sender_dir = FixtureDir::new("legacy-src");
    let (attachment_id, _body, _transport) = prepare_sender(&sender_dir);
    let file_id = ObjectId::parse(&blob_object_id_for(&attachment_id).expect("blob object id"))
        .expect("object id");
    let event = authenticated_event(
        group(),
        0,
        4,
        ApplicationContent::Attachment {
            attachment: AttachmentRef { file_id },
        },
    );
    // 发送方本机：旧路径（带解析端口）照旧落库。
    {
        let mut sender_core = sender_dir.core();
        let sender_id = attachment_id.clone();
        let applied = chat_core::attachment_message::apply_authenticated_application_event(
            &mut sender_core,
            &event,
            move |_file_id| Some(sender_id.clone()),
        )
        .expect("legacy reference still persists locally");
        assert_eq!(applied.attachment_ids, vec![attachment_id.clone()]);
    }
    // 独立库：没有描述符也无本地引用 → 拒绝，不留消息。
    {
        let mut core = sender_dir.core();
        let error = chat_core::attachment_message::apply_attachment_message_event(
            &mut core,
            &event,
            &attachment_id,
        )
        .expect_err("legacy reference is not an attachment descriptor event");
        assert_eq!(
            chat_core::attachment_message::attachment_message_error_code(&error),
            "ATTACHMENT_CORRUPTED"
        );
    }
    // 旧式引用在本机也没有 FileKey 时，下载/导出必须显式 ATTACHMENT_KEY_UNAVAILABLE。
    let receiver_dir = FixtureDir::new("legacy-dst");
    let attachments = receiver_dir.attachments();
    let error = attachments
        .encode_attachment_message_body(&attachment_id, &wrapping_key())
        .expect_err("no local record and no key");
    assert_eq!(error.code(), "ATTACHMENT_NOT_FOUND");
}
