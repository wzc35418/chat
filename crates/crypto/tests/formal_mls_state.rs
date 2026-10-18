//! CRYPTO-004/010 持久化切片：已提交 MLS 群状态落到正式加密库，重启后继续加解密。
//!
//! 覆盖：两设备建群 → 加密一条 → close → 同主密钥重开 → 继续用同一 group 加密，
//! 对端解密到原文；epoch 前进后旧状态不可覆盖/不可用于新消息；错误主密钥与篡改
//! 记录 STATE_CORRUPTED；写入失败不留下半状态（可继续用既有状态加解密）。

use chat_crypto::engine::{MlsDevice, MlsExportSummary};
use chat_storage::formal::{
    KeyProvenance, MlsGroupStateInput, ProtectedStorageKey, StorageError, Store, StoreOptions,
    open_store,
};
use std::fs;
use std::path::PathBuf;

struct Fixture {
    dir: PathBuf,
    alice: PathBuf,
    bob: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chat-crypto-mls-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&dir).expect("create fixture directory");
        let mut key_bytes = [0u8; 32];
        key_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            alice: dir.join("alice.db"),
            bob: dir.join("bob.db"),
            dir,
            key_bytes,
        }
    }

    fn key(&self) -> ProtectedStorageKey {
        ProtectedStorageKey::from_bytes(self.key_bytes)
    }

    fn other_key(&self) -> ProtectedStorageKey {
        let mut other = self.key_bytes;
        other[0] ^= 0xff;
        ProtectedStorageKey::from_bytes(other)
    }

    fn open(&self, path: &PathBuf) -> Store {
        open_store(StoreOptions::new(path, KeyProvenance::Raw32), self.key())
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

fn import_expect_err(store: &Store, group_key: &str, reason: &str) -> anyhow::Error {
    match MlsDevice::import_from_store(store, group_key) {
        Ok(_) => panic!("{reason}"),
        Err(error) => error,
    }
}

fn error_code(error: &anyhow::Error) -> String {
    if let Some(storage) = error.downcast_ref::<StorageError>() {
        return storage.code().to_owned();
    }
    format!("OTHER: {error}")
}

#[test]
fn committed_group_survives_close_and_reopen() {
    let fixture = Fixture::new("reopen");
    let (alice, bob) = pair();
    let group_key = alice.group_key().expect("group key");
    assert_eq!(alice.epoch().expect("epoch"), 1);

    // 关闭前把双方的已提交状态与 provider 秘密写入各自的正式加密库。
    let summary: MlsExportSummary = {
        let mut store = fixture.open(&fixture.alice);
        alice.export_to_store(&mut store).expect("export alice")
    };
    assert_eq!(summary.epoch, 1);
    assert!(summary.records > 0, "provider records must be persisted");
    {
        let mut store = fixture.open(&fixture.bob);
        bob.export_to_store(&mut store).expect("export bob");
    }
    // 模拟进程退出：设备与库句柄全部释放。
    drop(alice);
    drop(bob);

    // 同主密钥重开，从正式库恢复两个设备。
    let mut alice = {
        let store = fixture.open(&fixture.alice);
        MlsDevice::import_from_store(&store, &group_key).expect("import alice")
    };
    assert_eq!(alice.epoch().expect("epoch"), 1);
    assert_eq!(alice.group_key().expect("group key"), group_key);
    let ciphertext = alice.encrypt("重启后继续加密 🚀").expect("encrypt");
    assert!(
        !ciphertext
            .windows("重启后继续加密".len())
            .any(|window| window == "重启后继续加密".as_bytes()),
        "ciphertext must not carry plaintext"
    );

    let mut bob = {
        let store = fixture.open(&fixture.bob);
        MlsDevice::import_from_store(&store, &group_key).expect("import bob")
    };
    assert_eq!(bob.decrypt(&ciphertext, "Alice").expect("decrypt"), "重启后继续加密 🚀");

    // 反方向同样可用：Bob 加密 → Alice 解密。
    let reply = bob.encrypt("收到").expect("encrypt reply");
    assert_eq!(alice.decrypt(&reply, "Bob").expect("decrypt reply"), "收到");

    // 再导出一次（epoch 不变）是幂等快照替换，不产生新 epoch。
    let mut alice_store = fixture.open(&fixture.alice);
    let again = alice
        .export_to_store(&mut alice_store)
        .expect("re-export alice");
    assert_eq!(again.epoch, 1);
    assert!(again.state_version >= 2);
    assert_eq!(
        MlsDevice::import_from_store(&alice_store, &group_key)
            .expect("import again")
            .epoch()
            .expect("epoch"),
        1
    );
}

#[test]
fn epoch_advance_makes_previous_state_stale() {
    let fixture = Fixture::new("epoch");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let mut store = fixture.open(&fixture.alice);
    let first = alice.export_to_store(&mut store).expect("export epoch 1");
    assert_eq!(first.epoch, 1);

    // 推进 epoch（本地自更新 commit）。
    let advanced = alice
        .advance_epoch_with_self_update()
        .expect("advance epoch");
    assert_eq!(advanced, 2);
    let second = alice.export_to_store(&mut store).expect("export epoch 2");
    assert_eq!(second.epoch, 2);
    assert_eq!(second.state_version, first.state_version + 1);

    // 旧 epoch 快照不得覆盖新 epoch。
    let stale = store.save_mls_group_state(MlsGroupStateInput {
        group_id: group_key.clone(),
        epoch: 1,
        state: b"stale-header".to_vec(),
        records: Vec::new(),
    });
    let error = stale.expect_err("stale epoch must be rejected");
    assert_eq!(error.code(), "STALE_EPOCH");
    assert_eq!(
        store
            .load_mls_group_state(&group_key)
            .expect("load")
            .expect("present")
            .epoch,
        2,
        "the store keeps the advanced epoch"
    );

    // 重开后恢复的是 epoch 2，而不是旧状态。
    drop(store);
    let store = fixture.open(&fixture.alice);
    let mut alice = MlsDevice::import_from_store(&store, &group_key).expect("import");
    assert_eq!(alice.epoch().expect("epoch"), 2);

    // 旧 epoch 的设备无法解密新 epoch 的消息（旧状态不可用于新消息）。
    let ciphertext = alice.encrypt("epoch 2 消息").expect("encrypt");
    let failure = bob
        .decrypt(&ciphertext, "Alice")
        .expect_err("epoch 1 device must not decrypt epoch 2 messages");
    assert!(!failure.to_string().is_empty());
    drop(store);
}

#[test]
fn wrong_master_key_and_tampered_records_are_state_corrupted() {
    let fixture = Fixture::new("tamper");
    let (alice, _bob) = pair();
    let group_key = alice.group_key().expect("group key");
    {
        let mut store = fixture.open(&fixture.alice);
        alice.export_to_store(&mut store).expect("export");
    }

    // 错误主密钥：拒绝打开，不覆盖数据。
    let error = match open_store(
        StoreOptions::new(&fixture.alice, KeyProvenance::Raw32),
        fixture.other_key(),
    ) {
        Ok(_) => panic!("wrong key must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STATE_CORRUPTED");

    // 篡改 provider 记录：导入必须显式失败。
    {
        let store = fixture.open(&fixture.alice);
        store
            .connection()
            .execute(
                "UPDATE mls_record SET protected_value = X'00ff00ff' WHERE group_id = ?1",
                [&group_key],
            )
            .expect("tamper record");
    }
    let store = fixture.open(&fixture.alice);
    let error = import_expect_err(&store, &group_key, "tampered record must fail");
    assert_eq!(error_code(&error), "STATE_CORRUPTED");

    // 篡改群状态：同样显式失败。
    let fixture = Fixture::new("tamper-state");
    let (alice, _bob) = pair();
    let group_key = alice.group_key().expect("group key");
    {
        let mut store = fixture.open(&fixture.alice);
        alice.export_to_store(&mut store).expect("export");
        store
            .connection()
            .execute(
                "UPDATE mls_group_state SET protected_state = X'00ff00ff' WHERE group_id = ?1",
                [&group_key],
            )
            .expect("tamper state");
    }
    let store = fixture.open(&fixture.alice);
    let error = import_expect_err(&store, &group_key, "tampered state must fail");
    assert_eq!(error_code(&error), "STATE_CORRUPTED");

    // 不存在的群：明确失败，不新建空群。
    let error = import_expect_err(&store, "group-missing", "missing group must fail");
    assert!(!error.to_string().is_empty());
}

#[test]
fn failed_export_keeps_the_previous_complete_state() {
    let fixture = Fixture::new("half-state");
    let (alice, _bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let mut store = fixture.open(&fixture.alice);
    let first = alice.export_to_store(&mut store).expect("export epoch 1");

    // 用非法输入触发写入失败（群 ID 与 epoch 都合法，但状态为空 → 有界拒绝）。
    let error = store
        .save_mls_group_state(MlsGroupStateInput {
            group_id: group_key.clone(),
            epoch: 2,
            state: Vec::new(),
            records: Vec::new(),
        })
        .expect_err("empty state must be rejected");
    assert_eq!(error.code(), "INVALID_INPUT");

    // 上一次提交仍然完整可用。
    let loaded = store
        .load_mls_group_state(&group_key)
        .expect("load")
        .expect("present");
    assert_eq!(loaded.epoch, 1);
    assert_eq!(loaded.state_version, first.state_version);
    let restored =
        MlsDevice::import_from_store(&store, &group_key).expect("import after failed export");
    assert_eq!(restored.epoch().expect("epoch"), 1);
    assert!(!restored.group_key().expect("group key").is_empty());
}