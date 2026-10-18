//! 消息落库自动登记附件引用（ATTACHMENTS-008 引用计数）的集成测试。

use chat_core::attachments::{AttachmentError, FileToken, PersistedAttachmentStore};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, VerifiedEntryInput,
    open_store};
use std::fs;
use std::path::{Path, PathBuf};

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chat-core-attrefs-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn open(&self) -> PersistedAttachmentStore {
        let store = open_store(
            StoreOptions::new(self.path("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes([9u8; 32]),
        )
        .expect("open formal store");
        PersistedAttachmentStore::open(self.path("cache"), store).expect("open attachment store")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn entry(message_id: &str) -> VerifiedEntryInput {
    VerifiedEntryInput {
        conversation_id: "conv-1".to_owned(),
        title: "附件引用测试".to_owned(),
        message_id: message_id.to_owned(),
        author_account: "acct-1".to_owned(),
        author_device: "dev-1".to_owned(),
        body: "带附件的消息正文".to_owned(),
        mine: true,
        created_at_ms: 1_700_000_000_000,
    }
}

fn import(store: &PersistedAttachmentStore, dir: &Path, name: &str) -> String {
    let source = dir.join(name);
    fs::write(&source, format!("attachment body for {name}")).expect("write attachment source");
    store
        .import_file(&FileToken::from_local_path(&source), name)
        .expect("import attachment")
        .attachment_id
}

#[test]
fn message_persistence_registers_attachment_refs_idempotently() {
    let dir = Fixture::new("register");
    let store = dir.open();
    let attachment_a = import(&store, &dir.dir, "a.bin");
    let attachment_b = import(&store, &dir.dir, "b.bin");

    let applied = store
        .apply_verified_entry_with_attachments(
            entry("msg-1"),
            vec![attachment_a.clone(), attachment_b.clone()],
        )
        .expect("apply entry with attachments");
    assert_eq!(applied.message_id, "msg-1");
    assert!(!applied.already_existed);
    assert!(store.message_exists("msg-1").expect("message exists"));
    let mut expected = vec![attachment_a.clone(), attachment_b.clone()];
    expected.sort();
    assert_eq!(
        store.attachments_for_message("msg-1").expect("message attachments"),
        expected,
        "the message references exactly the registered attachments"
    );
    // 反向查询同样成立。
    assert_eq!(
        store
            .attachments_for_message("msg-1")
            .expect("query")
            .len(),
        2
    );

    // 重复落库：幂等，引用不重复登记。
    let replay = store
        .apply_verified_entry_with_attachments(
            entry("msg-1"),
            vec![attachment_a.clone(), attachment_b.clone()],
        )
        .expect("idempotent replay");
    assert!(replay.already_existed);
    assert_eq!(
        store.attachments_for_message("msg-1").expect("query").len(),
        2,
        "replay must not duplicate references"
    );

    // 未被引用的第三条附件不会凭空出现。
    let attachment_c = import(&store, &dir.dir, "c.bin");
    assert!(!store
        .attachments_for_message("msg-1")
        .expect("query")
        .contains(&attachment_c));
}

#[test]
fn invalid_attachment_reference_rolls_back_the_whole_entry() {
    let dir = Fixture::new("rollback");
    let store = dir.open();
    let attachment_a = import(&store, &dir.dir, "a.bin");
    let missing = "att-0123456789abcdef0123456789abcdef";

    let error = store
        .apply_verified_entry_with_attachments(entry("msg-bad"), vec![attachment_a.clone(), missing.to_owned()])
        .expect_err("a missing attachment must fail the write");
    assert_eq!(error.code(), "ATTACHMENT_NOT_FOUND");
    assert!(
        !store.message_exists("msg-bad").expect("message exists"),
        "the message must not be half-written"
    );
    assert!(
        store
            .attachments_for_message("msg-bad")
            .expect("query")
            .is_empty(),
        "no reference may survive a failed write"
    );

    // 修正后重试成功（证明失败没有破坏可用性）。
    let applied = store
        .apply_verified_entry_with_attachments(entry("msg-bad"), vec![attachment_a.clone()])
        .expect("retry with a valid attachment");
    assert_eq!(applied.message_id, "msg-bad");
    assert_eq!(
        store.attachments_for_message("msg-bad").expect("query"),
        vec![attachment_a]
    );
}

#[test]
fn deleting_the_message_releases_the_reference() {
    let dir = Fixture::new("delete");
    let store = dir.open();
    let attachment = import(&store, &dir.dir, "a.bin");
    store
        .apply_verified_entry_with_attachments(entry("msg-del"), vec![attachment.clone()])
        .expect("apply entry");
    assert_eq!(
        store.attachments_for_message("msg-del").expect("query"),
        vec![attachment.clone()]
    );
    assert!(store.delete_message("msg-del").expect("delete message"));
    assert!(!store.delete_message("msg-del").expect("delete again"));
    assert!(
        store
            .attachments_for_message("msg-del")
            .expect("query")
            .is_empty(),
        "references cascade away with the message"
    );
    // 附件记录本身仍在（是否可回收由 ATTACHMENTS-008 的记录+消息引用共同判断）。
    assert!(store
        .attachment_record(&attachment)
        .expect("record")
        .is_some());
}

#[test]
fn attachment_error_maps_storage_failures() {
    // 引用不存在附件的错误码透传为 ATTACHMENT_NOT_FOUND（不是泛化的 INTERNAL_FAILURE）。
    let dir = Fixture::new("codes");
    let store = dir.open();
    let error: AttachmentError = store
        .apply_verified_entry_with_attachments(
            entry("msg-code"),
            vec!["att-ffffffffffffffffffffffffffffffff".to_owned()],
        )
        .expect_err("missing attachment");
    assert_eq!(error.code(), "ATTACHMENT_NOT_FOUND");
}
