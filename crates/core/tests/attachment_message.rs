//! 对端带附件应用消息的端到端用例（真 OpenMLS + 真临时加密库）。
//!
//! 覆盖：带附件事件 → 消息落库且 attachments_for_message 返回该附件、群快照（epoch/成员）不变；
//! 未知附件 → 整条消息拒绝且消息不存在；重复处理幂等；纯文本事件同样落库。
//!
//! 事件用**真实协议层**构造：真实设备签名候选（PROTOCOL-005 verify_candidate）+ 真实内层事件
//! 编码与绑定校验（PROTOCOL-009 verify_inner_event），因此用例覆盖“已验证事件 → 落库”全链路。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::attachment_message::{
    ATTACHMENT_ONLY_BODY, AppliedApplicationMessage, apply_authenticated_application_event,
    attachment_message_error_code, message_id_for_event,
};
use chat_core::attachment_transport::blob_object_id_for;
use chat_core::attachments::{FileToken, PersistedAttachmentStore};
use chat_core::groups::{CreateGroupRequest, GroupBootstrapPeer, GroupView, create_group};
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
use ed25519_dalek::SigningKey;

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const AUTHOR_SEED: [u8; 32] = [0x41; 32];

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-attmsg-{}-{nanos}-{counter}",
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
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn conversation() -> ConversationId {
    ConversationId::from_bytes([0x11; 32])
}

fn author() -> (AccountId, DeviceId, SigningKey) {
    (
        AccountId::from_bytes([0x31; 32]),
        DeviceId::from_bytes([0x51; 32]),
        signing_key_from_seed(&AUTHOR_SEED),
    )
}

/// 用真实协议层构造一个“已验证应用事件”。
fn authenticated_event(
    group_id: GroupId,
    mls_epoch: u64,
    event_id: u8,
    content: ApplicationContent,
    sent_at_unix_ms: u64,
) -> AuthenticatedEvent {
    let (account_id, device_id, signing_key) = author();
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
        candidate_signing_bytes(&unsigned)
            .expect("signing bytes")
            .as_bytes(),
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
        sent_at_unix_ms,
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

/// 夹具：真群（alice + bob）+ 一个已导入的本地附件 + 打开的内核。
struct Fixture {
    dir: FixtureDir,
    core: ConversationCore,
    attachment_id: String,
    group: GroupView,
}

impl Fixture {
    fn new() -> Self {
        let dir = FixtureDir::new();
        // 1) 先导入一个真实附件（PersistedAttachmentStore 独占写库，导入后立即释放）。
        let attachment_id = {
            let store = open_store(
                StoreOptions::new(dir.database(), KeyProvenance::Raw32),
                ProtectedStorageKey::from_bytes(MASTER_KEY),
            )
            .expect("open store for attachment import");
            let attachments = PersistedAttachmentStore::open(dir.cache(), store)
                .expect("open persisted attachment store");
            let source = dir.write("photo.bin", b"ATTACHMENT-PLAINTEXT-CONTENT");
            attachments
                .import_file(&FileToken::from_local_path(&source), "photo.bin")
                .expect("import attachment")
                .attachment_id
        };
        // 2) 建一个真群（写群快照与 MLS 状态），随后释放写句柄。
        let group = {
            let mut store = open_store(
                StoreOptions::new(dir.database(), KeyProvenance::Raw32),
                ProtectedStorageKey::from_bytes(MASTER_KEY),
            )
            .expect("open store for group creation");
            let mut alice = MlsDevice::new("alice").expect("alice");
            let bob = MlsDevice::new("bob").expect("bob");
            let request = CreateGroupRequest::new("附件消息群", "alice");
            let peer = GroupBootstrapPeer {
                credential: "bob".to_owned(),
                device: &bob,
            };
            create_group(&mut alice, &peer, &mut store, &request).expect("create group")
        };
        // 3) 打开内核（同一个库），并确认快照可读。
        let core =
            ConversationCore::open(&dir.database_text(), &master_key_hex()).expect("open core");
        let snapshot = core.group_snapshot(&group.group_id).expect("snapshot");
        assert_eq!(snapshot.epoch, group.epoch);
        Self {
            dir,
            core,
            attachment_id,
            group,
        }
    }

    fn group_id(&self) -> GroupId {
        GroupId::parse(&self.group.group_id).expect("group id text")
    }

    fn file_id(&self) -> ObjectId {
        ObjectId::parse(&blob_object_id_for(&self.attachment_id).expect("blob object id"))
            .expect("object id text")
    }

    /// 会话里的消息数；会话行都还没建（一条都没落库）时按 0 计。
    fn message_count(&self) -> usize {
        match self.core.list_messages(&conversation().encode(), None, 50) {
            Ok(page) => page.items.len(),
            Err(_) => 0,
        }
    }

    fn snapshot(&self) -> GroupView {
        self.core
            .group_snapshot(&self.group.group_id)
            .expect("snapshot")
    }
}

#[test]
fn attachment_event_persists_the_message_and_the_reference() {
    let mut fixture = Fixture::new();
    let before = fixture.snapshot();
    let file_id = fixture.file_id();
    let attachment_id = fixture.attachment_id.clone();
    let event = authenticated_event(
        fixture.group_id(),
        before.epoch,
        0xe1,
        ApplicationContent::Attachment {
            attachment: AttachmentRef::new(file_id),
        },
        1_700_000_000_000,
    );
    let applied: AppliedApplicationMessage =
        apply_authenticated_application_event(&mut fixture.core, &event, |candidate| {
            if candidate == file_id {
                Some(attachment_id.clone())
            } else {
                None
            }
        })
        .expect("apply attachment event");

    assert_eq!(applied.message_id, message_id_for_event(&event));
    assert!(applied.message_id.starts_with("msg:1:"));
    assert_eq!(applied.body, ATTACHMENT_ONLY_BODY);
    assert_eq!(applied.attachment_ids, vec![attachment_id.clone()]);
    assert!(!applied.mine, "the peer device is not the local identity");
    assert_eq!(applied.sent_at_ms, 1_700_000_000_000);
    assert_eq!(applied.conversation_id, conversation().encode());

    // 引用登记可见：attachments_for_message 返回该附件。
    assert_eq!(
        fixture
            .core
            .attachments_for_message(&applied.message_id)
            .expect("message attachments"),
        vec![attachment_id]
    );
    // 消息真的落库了（会话分页里恰好一条）。
    assert_eq!(fixture.message_count(), 1);

    // 群快照不因应用消息而改变（epoch/成员/策略版本全等）。
    assert_eq!(
        fixture.snapshot(),
        before,
        "application messages do not touch the group snapshot"
    );

    // 重复处理同一个事件：幂等（同一消息 ID、引用不重复、消息数不变）。
    let resolved = fixture.attachment_id.clone();
    let replay =
        apply_authenticated_application_event(&mut fixture.core, &event, |_| Some(resolved.clone()))
            .expect("replay is idempotent");
    assert_eq!(replay.message_id, applied.message_id);
    assert_eq!(
        fixture
            .core
            .attachments_for_message(&replay.message_id)
            .expect("attachments after replay")
            .len(),
        1
    );
    assert_eq!(fixture.message_count(), 1);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn unknown_attachment_rejects_the_whole_message() {
    let mut fixture = Fixture::new();
    let before = fixture.snapshot();
    let file_id = fixture.file_id();
    let event = authenticated_event(
        fixture.group_id(),
        before.epoch,
        0xe2,
        ApplicationContent::Attachment {
            attachment: AttachmentRef::new(file_id),
        },
        1_700_000_100_000,
    );
    let message_id = message_id_for_event(&event);

    // 1) 解析不到本地附件 → 写库之前就拒绝。
    let error = apply_authenticated_application_event(&mut fixture.core, &event, |_| None)
        .expect_err("unknown attachment reference");
    assert_eq!(
        attachment_message_error_code(&error),
        "ATTACHMENT_NOT_FOUND"
    );
    assert_eq!(fixture.message_count(), 0, "no message was persisted");
    let refs = fixture.core.attachments_for_message(&message_id);
    assert!(
        refs.as_ref().map(|ids| ids.is_empty()).unwrap_or(true),
        "no reference was registered: {refs:?}"
    );

    // 2) 解析到一个正式库里不存在的本地 ID → storage 事务整体回滚（同样不留半落库）。
    let missing = "att-00000000000000000000000000000000".to_owned();
    let error =
        apply_authenticated_application_event(&mut fixture.core, &event, |_| Some(missing.clone()))
            .expect_err("unknown local attachment id");
    assert!(!attachment_message_error_code(&error).is_empty());
    assert_eq!(fixture.message_count(), 0);
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn text_event_persists_without_attachment_refs() {
    let mut fixture = Fixture::new();
    let before = fixture.snapshot();
    let event = authenticated_event(
        fixture.group_id(),
        before.epoch,
        0xe3,
        ApplicationContent::Text {
            text: "对端的纯文本消息".to_owned(),
        },
        1_700_000_200_000,
    );
    let applied = apply_authenticated_application_event(&mut fixture.core, &event, |_| None)
        .expect("apply text event");
    assert_eq!(applied.body, "对端的纯文本消息");
    assert!(applied.attachment_ids.is_empty());
    assert_eq!(
        fixture
            .core
            .attachments_for_message(&applied.message_id)
            .expect("no attachments")
            .len(),
        0
    );
    assert_eq!(fixture.message_count(), 1);
    assert_eq!(fixture.snapshot(), before);
    let _ = fixture.dir.database();
}
