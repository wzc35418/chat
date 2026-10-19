//! MLS exporter 密钥派生用例：双设备同 epoch 一致、epoch 前进后变化、输入有界、
//! 以及 exporter 密钥与描述符的 epoch 绑定。

use chat_crypto::control::{
    CommitExpectation, ControlOperation, VerifiedGroupEntry,
};
use chat_crypto::engine::MlsDevice;
use chat_crypto::exporter::{
    EXPORTER_LABEL_ATTACHMENT_FILE_KEY, MAX_EXPORTER_CONTEXT_BYTES, attachment_wrapping_context,
};
use chat_crypto::secret_descriptor::{open_secret_descriptor, seal_secret_descriptor};
use chat_storage::formal::{
    KeyProvenance, ProtectedStorageKey, Store as FormalStore, StoreOptions, open_store,
};
use std::fs;
use std::path::PathBuf;

struct Fixture {
    dir: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chat-crypto-exporter-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create fixture dir");
        let mut key_bytes = [0u8; 32];
        key_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self { dir, key_bytes }
    }

    fn open(&self, device: &str) -> FormalStore {
        open_store(
            StoreOptions::new(self.dir.join(format!("{device}.db")), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(self.key_bytes),
        )
        .expect("open formal store")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn pair() -> (MlsDevice, MlsDevice) {
    let mut alice = MlsDevice::new("Alice").expect("alice");
    let mut bob = MlsDevice::new("Bob").expect("bob");
    let welcome = alice
        .create_and_invite(&bob.key_package().expect("bob key package"), &bob)
        .expect("create group");
    bob.join(&welcome, &alice).expect("bob joins");
    (alice, bob)
}

fn commit_entry(group_id: &str, epoch: u64, sender: &str, payload: Vec<u8>) -> VerifiedGroupEntry {
    VerifiedGroupEntry {
        group_id: group_id.to_owned(),
        epoch,
        sender_identity: sender.to_owned(),
        expectation: Some(CommitExpectation {
            added: 0,
            removed: 0,
            updated: 0,
        }),
        payload,
    }
}

#[test]
fn exporter_key_is_identical_across_members_in_the_same_epoch() {
    let fixture = Fixture::new("same-epoch");
    let (alice, bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let mut alice_store = fixture.open("alice");
    let mut bob_store = fixture.open("bob");
    alice.export_to_store(&mut alice_store).expect("export alice");
    bob.export_to_store(&mut bob_store).expect("export bob");

    let context = attachment_wrapping_context(
        "att-00112233445566778899aabbccddeeff",
        &group_key,
        1,
    );
    let alice_key = alice
        .export_key(
            &alice_store,
            EXPORTER_LABEL_ATTACHMENT_FILE_KEY,
            &context,
            32,
        )
        .expect("alice exports");
    let bob_key = bob
        .export_key(&bob_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &context, 32)
        .expect("bob exports");
    assert_eq!(
        alice_key.as_slice(),
        bob_key.as_slice(),
        "same group + same epoch + same context must derive the same key"
    );
    assert_eq!(alice_key.len(), 32);
    // 不同 context 派生不同密钥。
    let other_context = attachment_wrapping_context("att-ffffffffffffffffffffffffffffffff", &group_key, 1);
    let other = alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &other_context, 32)
        .expect("export other context");
    assert_ne!(alice_key.as_slice(), other.as_slice());
}

#[test]
fn exporter_key_changes_with_the_epoch_and_binds_descriptors() {
    let fixture = Fixture::new("epoch-advance");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let mut alice_store = fixture.open("alice");
    let mut bob_store = fixture.open("bob");
    alice.export_to_store(&mut alice_store).expect("export alice");
    bob.export_to_store(&mut bob_store).expect("export bob");

    let context = attachment_wrapping_context("att-epoch", &group_key, 1);
    let epoch1_key = alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &context, 32)
        .expect("epoch 1 export");

    // 用 epoch 1 的密钥封装描述符。
    let file_key = chat_crypto::attachment::FileKey::generate();
    let descriptor = seal_secret_descriptor(&epoch1_key.as_slice().try_into().expect("32 bytes"), "att-epoch", &file_key)
        .expect("seal with epoch 1 key");

    // Alice 自更新推进到 epoch 2，Bob 处理该提交后也到 epoch 2。
    let pending = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut alice_store)
        .expect("prepare self update");
    alice
        .merge_accepted_pending(&pending.acceptance(), &mut alice_store)
        .expect("merge self update");
    bob.process_verified_entry(&commit_entry(&group_key, 1, "Alice", pending.commit.clone()))
        .expect("bob applies the self update");
    bob.export_to_store(&mut bob_store).expect("persist bob epoch 2");
    assert_eq!(alice.epoch().expect("alice epoch"), 2);
    assert_eq!(bob.epoch().expect("bob epoch"), 2);

    let epoch2_context = attachment_wrapping_context("att-epoch", &group_key, 2);
    let epoch2_key = alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &epoch2_context, 32)
        .expect("epoch 2 export");
    assert_ne!(epoch1_key.as_slice(), epoch2_key.as_slice(), "epoch advance changes the key");
    // 双设备在 epoch 2 仍一致。
    let bob_epoch2 = bob
        .export_key(&bob_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &epoch2_context, 32)
        .expect("bob epoch 2 export");
    assert_eq!(epoch2_key.as_slice(), bob_epoch2.as_slice());
    // 旧 epoch 密钥不能解新描述符，反之亦然。
    let next = seal_secret_descriptor(&epoch2_key.as_slice().try_into().expect("32 bytes"), "att-epoch", &file_key)
        .expect("seal with epoch 2 key");
    assert!(open_secret_descriptor(&epoch1_key.as_slice().try_into().expect("32 bytes"), "att-epoch", &next).is_err());
    assert!(open_secret_descriptor(&epoch2_key.as_slice().try_into().expect("32 bytes"), "att-epoch", &next).is_ok());
    assert!(open_secret_descriptor(&epoch2_key.as_slice().try_into().expect("32 bytes"), "att-epoch", &descriptor).is_err());
}

#[test]
fn exporter_rejects_unlisted_labels_long_contexts_and_bad_lengths() {
    let fixture = Fixture::new("bounds");
    let (alice, _bob) = pair();
    let mut alice_store = fixture.open("alice");
    alice.export_to_store(&mut alice_store).expect("export");
    let context = b"ok".to_vec();

    let label_error = alice
        .export_key(&alice_store, "chat unknown label", &context, 32)
        .expect_err("unlisted label must be rejected");
    assert_eq!(label_error.code(), "UNSUPPORTED_EXPORTER_LABEL");

    let long_context = vec![0u8; MAX_EXPORTER_CONTEXT_BYTES + 1];
    let context_error = alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &long_context, 32)
        .expect_err("oversized context must be rejected");
    assert_eq!(context_error.code(), "EXPORTER_CONTEXT_TOO_LONG");

    let short_error = alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &context, 0)
        .expect_err("zero-length key must be rejected");
    assert_eq!(short_error.code(), "EXPORTER_KEY_LENGTH_INVALID");
    let long_error = alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &context, 33)
        .expect_err("oversized key must be rejected");
    assert_eq!(long_error.code(), "EXPORTER_KEY_LENGTH_INVALID");
    // 上限值本身可用。
    assert_eq!(
        alice
            .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &context, 32)
            .expect("max length works")
            .len(),
        32
    );
}

#[test]
fn exporter_requires_a_persisted_epoch_and_no_pending_commit() {
    let fixture = Fixture::new("state");
    let (mut alice, _bob) = pair();
    let mut alice_store = fixture.open("alice");
    let context = b"state".to_vec();

    // 还没有持久化快照：拒绝导出（不能拿未落库的 epoch 密钥封装）。
    let not_persisted = alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &context, 32)
        .expect_err("an unpersisted epoch must not export");
    assert_eq!(not_persisted.code(), "EXPORTER_NOT_PERSISTED");

    alice.export_to_store(&mut alice_store).expect("export epoch 1");
    assert!(alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &context, 32)
        .is_ok());

    // 有 pending 控制提交时拒绝导出。
    let _pending = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut alice_store)
        .expect("prepare self update");
    let pending_error = alice
        .export_key(&alice_store, EXPORTER_LABEL_ATTACHMENT_FILE_KEY, &context, 32)
        .expect_err("a pending commit blocks exporter derivation");
    assert_eq!(pending_error.code(), "EXPORTER_PENDING_COMMIT");
}
