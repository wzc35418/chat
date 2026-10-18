//! CRYPTO-005~009 群成员控制集成测试（真实 OpenMLS + 正式加密存储快照）。
//!
//! 覆盖：建群 → 加成员（Commit + Welcome）→ Welcome 加入 → 三方加解密往返；
//! pending 未接受时 epoch 不前进、新 epoch 消息不可解密；拒绝后 discard 继续旧 epoch；
//! 关闭重开后 pending / 已提交状态正确；错误/乱序/重复/来源不符条目显式拒绝；
//! 控制结果与预期不一致时停止该群分支；移除成员的提交在双方一致生效。

use chat_crypto::control::{
    CommitExpectation, ControlAcceptance, ControlOperation, ControlOperationKind, ControlRejection,
    PendingControl, ProcessedEntry, VerifiedGroupEntry, WelcomeAcceptance,
};
use chat_crypto::engine::MlsDevice;
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
            "chat-crypto-control-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&dir).expect("create fixture directory");
        let mut key_bytes = [0u8; 32];
        key_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self { dir, key_bytes }
    }

    fn open(&self, device: &str) -> FormalStore {
        let path = self.dir.join(format!("{device}.db"));
        open_store(
            StoreOptions::new(path, KeyProvenance::Raw32),
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

fn error_code(error: &chat_crypto::control::ControlError) -> &'static str {
    error.code()
}

fn app_entry(group_id: &str, epoch: u64, sender: &str, payload: Vec<u8>) -> VerifiedGroupEntry {
    VerifiedGroupEntry {
        group_id: group_id.to_owned(),
        epoch,
        sender_identity: sender.to_owned(),
        expectation: None,
        payload,
    }
}

fn commit_entry(
    group_id: &str,
    epoch: u64,
    sender: &str,
    payload: Vec<u8>,
    expectation: CommitExpectation,
) -> VerifiedGroupEntry {
    VerifiedGroupEntry {
        group_id: group_id.to_owned(),
        epoch,
        sender_identity: sender.to_owned(),
        expectation: Some(expectation),
        payload,
    }
}

fn self_update_expectation() -> CommitExpectation {
    CommitExpectation {
        added: 0,
        removed: 0,
        updated: 0,
    }
}

/// 建一个 2 人组，返回 Alice 已准备（未接受）的 Add(Carol) 提交。
fn pending_add_carol(
    fixture: &Fixture,
) -> (
    MlsDevice,
    MlsDevice,
    MlsDevice,
    PendingControl,
    FormalStore,
    FormalStore,
) {
    let (mut alice, bob) = pair();
    let carol = MlsDevice::new("Carol").expect("carol");
    let mut alice_store = fixture.open("alice");
    alice
        .export_to_store(&mut alice_store)
        .expect("export epoch 1");
    let pending = alice
        .prepare_control(
            ControlOperation::AddMembers {
                key_packages: vec![carol.key_package().expect("carol key package")],
            },
            &mut alice_store,
        )
        .expect("prepare add");
    let carol_store = fixture.open("carol");
    (alice, bob, carol, pending, alice_store, carol_store)
}

#[test]
fn add_member_welcome_join_and_bidirectional_roundtrip() {
    let fixture = Fixture::new("add");
    let (mut alice, mut bob, mut carol, pending, mut alice_store, mut carol_store) =
        pending_add_carol(&fixture);
    let group_key = alice.group_key().expect("group key");
    let mut bob_store = fixture.open("bob");
    bob.export_to_store(&mut bob_store).expect("export bob epoch 1");

    // 准备阶段不得推进 epoch，pending 分支落在 base_epoch。
    assert_eq!(alice.epoch().expect("epoch"), 1);
    assert_eq!(pending.base_epoch, 1);
    assert_eq!(pending.target_epoch, 2);
    assert_eq!(pending.operation, ControlOperationKind::AddMembers);
    assert_eq!(pending.added, 1);
    assert_eq!(pending.removed, 0);
    assert!(pending.welcome.is_some(), "add members must produce a welcome");
    assert_eq!(
        alice_store
            .load_mls_group_state(&group_key)
            .expect("load")
            .expect("present")
            .epoch,
        1,
        "the pending branch is persisted at the base epoch"
    );
    assert_eq!(
        alice.pending_control().expect("pending").expect("some").commit_digest,
        pending.commit_digest
    );

    // 加成员之前的 epoch 1 消息：新成员加入后不应具备旧历史解密能力。
    let old_history = alice.encrypt("加成员之前的历史消息").expect("encrypt");

    // Carol 用 Welcome 加入（epoch 2）。
    let joined = carol
        .join_from_welcome(
            pending.welcome.as_ref().expect("welcome"),
            &WelcomeAcceptance {
                group_id: group_key.clone(),
                expected_epoch: 2,
                members: vec!["Alice".into(), "Bob".into(), "Carol".into()],
            },
            &mut carol_store,
        )
        .expect("carol joins");
    assert_eq!(joined.epoch, 2);
    assert_eq!(joined.members.len(), 3);

    // Alice 用接受证据合并，epoch 才前进。
    let merged = alice
        .merge_accepted_pending(&pending.acceptance(), &mut alice_store)
        .expect("merge accepted");
    assert_eq!(merged.old_epoch, 1);
    assert_eq!(merged.new_epoch, 2);
    assert_eq!(merged.operation, ControlOperationKind::AddMembers);
    assert!(alice.pending_control().expect("pending").is_none());
    assert_eq!(alice.epoch().expect("epoch"), 2);

    // 证据绑定：摘要/epoch 不符一律拒绝，且不改变 epoch、不消耗 pending。
    let mut wrong_digest = pending.acceptance();
    wrong_digest.commit_digest[0] ^= 0xff;
    assert_eq!(
        alice
            .merge_accepted_pending(&wrong_digest, &mut alice_store)
            .unwrap_err()
            .code(),
        "PENDING_MISMATCH"
    );
    let mut wrong_target = pending.acceptance();
    wrong_target.target_epoch += 1;
    assert_eq!(
        alice
            .merge_accepted_pending(&wrong_target, &mut alice_store)
            .unwrap_err()
            .code(),
        "PENDING_MISMATCH"
    );
    // 重复合并同一提交：pending 已被消费，必须显式失败而不是再消费一次。
    assert_eq!(
        alice
            .merge_accepted_pending(&pending.acceptance(), &mut alice_store)
            .unwrap_err()
            .code(),
        "PENDING_MISMATCH"
    );
    assert_eq!(alice.epoch().expect("epoch"), 2);

    // Bob 处理对端 Commit，成员变化与预期一致。
    let processed = bob
        .process_verified_entry(&commit_entry(
            &group_key,
            1,
            "Alice",
            pending.commit.clone(),
            CommitExpectation {
                added: 1,
                removed: 0,
                updated: 0,
            },
        ))
        .expect("bob processes commit");
    match processed {
        ProcessedEntry::Commit {
            sender_identity,
            old_epoch,
            new_epoch,
            added,
            removed,
            updated,
            self_removed,
        } => {
            assert_eq!(sender_identity, "Alice");
            assert_eq!(old_epoch, 1);
            assert_eq!(new_epoch, 2);
            assert_eq!(added, 1);
            assert_eq!(removed, 0);
            assert_eq!(updated, 0);
            assert!(!self_removed);
        }
        other => panic!("expected a merged commit, got {other:?}"),
    }
    assert_eq!(bob.epoch().expect("epoch"), 2);

    // 三方加解密往返。
    let message = alice.encrypt("三方群消息").expect("encrypt");
    assert_eq!(bob.decrypt(&message, "Alice").expect("bob decrypt"), "三方群消息");
    assert_eq!(
        carol.decrypt(&message, "Alice").expect("carol decrypt"),
        "三方群消息"
    );
    let reply = carol.encrypt("Carol 回复").expect("encrypt");
    assert_eq!(alice.decrypt(&reply, "Carol").expect("decrypt"), "Carol 回复");
    assert!(
        carol.decrypt(&old_history, "Alice").is_err(),
        "a device that joined at epoch 2 must not decrypt epoch-1 history"
    );
    let bob_message = bob.encrypt("Bob 也回复").expect("encrypt");
    assert_eq!(
        carol.decrypt(&bob_message, "Bob").expect("decrypt"),
        "Bob 也回复"
    );
}

#[test]
fn pending_commit_does_not_advance_epoch_and_new_epoch_messages_are_rejected() {
    let fixture = Fixture::new("pending");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let mut alice_store = fixture.open("alice");
    alice.export_to_store(&mut alice_store).expect("export epoch 1");

    let pending = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut alice_store)
        .expect("prepare self update");
    assert_eq!(alice.epoch().expect("epoch"), 1, "pending must not advance the epoch");

    // 并发准备第二个控制提交必须被拒绝（只串行准备）。
    let error = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut alice_store)
        .unwrap_err();
    assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");
    assert!(matches!(
        error,
        chat_crypto::control::ControlError::PendingCommitExists { base_epoch: 1 }
    ));

    // 本机 pending 提交被节点回显时不得再次消费/合并（只能走 merge_accepted_pending）。
    assert_eq!(
        alice
            .process_verified_entry(&commit_entry(
                &group_key,
                1,
                "Alice",
                pending.commit.clone(),
                self_update_expectation(),
            ))
            .unwrap_err()
            .code(),
        "PENDING_MISMATCH"
    );
    assert!(alice.pending_control().expect("pending").is_some());
    assert_eq!(alice.epoch().expect("epoch"), 1);

    // pending 期间旧 epoch 仍可正常加解密（未合并 = 旧状态继续可用）。
    let old_epoch_message = alice.encrypt("pending 期间的旧 epoch 消息").expect("encrypt");
    assert_eq!(
        bob.decrypt(&old_epoch_message, "Alice").expect("decode"),
        "pending 期间的旧 epoch 消息"
    );

    // 接受并合并后 Alice 到 epoch 2；Bob 仍停在 epoch 1。
    alice
        .merge_accepted_pending(&pending.acceptance(), &mut alice_store)
        .expect("merge accepted");
    assert_eq!(alice.epoch().expect("epoch"), 2);
    assert_eq!(bob.epoch().expect("epoch"), 1);
    let new_epoch_message = alice.encrypt("epoch 2 消息").expect("encrypt");
    assert!(
        bob.decrypt(&new_epoch_message, "Alice").is_err(),
        "a device still at the old epoch must not decrypt new-epoch messages"
    );

    // Bob 处理该 Commit 后追上 epoch 2，可以解密后续消息。
    bob.process_verified_entry(&commit_entry(
        &group_key,
        1,
        "Alice",
        pending.commit.clone(),
        self_update_expectation(),
    ))
    .expect("process commit");
    assert_eq!(bob.epoch().expect("epoch"), 2);
    let after_commit = alice.encrypt("追上之后").expect("encrypt");
    assert_eq!(bob.decrypt(&after_commit, "Alice").expect("decrypt"), "追上之后");
}

#[test]
fn pending_and_committed_states_survive_close_and_reopen() {
    let fixture = Fixture::new("reopen");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let pending;
    {
        let mut store = fixture.open("alice");
        alice.export_to_store(&mut store).expect("export epoch 1");
        pending = alice
            .prepare_control(ControlOperation::SelfUpdate, &mut store)
            .expect("prepare self update");
        assert_eq!(
            store
                .load_mls_group_state(&group_key)
                .expect("load")
                .expect("present")
                .epoch,
            1
        );
    }
    drop(alice);

    // 重开后 pending 仍然存在（未提交态正确），epoch 未变。
    let mut store = fixture.open("alice");
    let mut alice = MlsDevice::import_from_store(&store, &group_key).expect("import pending");
    assert_eq!(alice.epoch().expect("epoch"), 1);
    let restored = alice
        .pending_control()
        .expect("pending")
        .expect("pending must survive reopen");
    assert_eq!(restored.base_epoch, 1);
    assert_eq!(restored.target_epoch, 2);
    assert_eq!(restored.commit_digest, pending.commit_digest);
    assert_eq!(restored.commit, pending.commit);

    // 用同一份接受证据继续合并，重开不会丢失 pending 候选。
    let merged = alice
        .merge_accepted_pending(&restored.acceptance(), &mut store)
        .expect("merge after reopen");
    assert_eq!(merged.new_epoch, 2);
    assert_eq!(
        store
            .load_mls_group_state(&group_key)
            .expect("load")
            .expect("present")
            .epoch,
        2
    );
    drop(store);
    drop(alice);

    // 已提交态正确：epoch 2、无 pending。
    let store = fixture.open("alice");
    let mut alice = MlsDevice::import_from_store(&store, &group_key).expect("import committed");
    assert_eq!(alice.epoch().expect("epoch"), 2);
    assert!(alice.pending_control().expect("pending").is_none());

    // Bob 追到 epoch 2 后双方继续往返。
    bob.process_verified_entry(&commit_entry(
        &group_key,
        1,
        "Alice",
        restored.commit.clone(),
        self_update_expectation(),
    ))
    .expect("process commit");
    let message = alice.encrypt("重开后的提交态").expect("encrypt");
    assert_eq!(bob.decrypt(&message, "Alice").expect("decrypt"), "重开后的提交态");
}

#[test]
fn rejected_pending_discards_and_old_epoch_still_works() {
    let fixture = Fixture::new("discard");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let mut store = fixture.open("alice");
    alice.export_to_store(&mut store).expect("export epoch 1");
    let pending = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut store)
        .expect("prepare self update");

    // 伪造的拒绝证据（摘要不符）不能丢弃 pending。
    let mut wrong = pending.rejection("coordinator rejected the commit");
    wrong.commit_digest[0] ^= 0xff;
    assert_eq!(
        alice.discard_rejected_pending(&wrong, &mut store).unwrap_err().code(),
        "PENDING_MISMATCH"
    );
    assert!(alice.pending_control().expect("pending").is_some());

    // 明确的拒绝证据：回滚到上一完整状态，epoch 不变。
    let discarded = alice
        .discard_rejected_pending(&pending.rejection("coordinator rejected the commit"), &mut store)
        .expect("discard pending");
    assert_eq!(discarded.epoch, 1);
    assert_eq!(discarded.operation, ControlOperationKind::SelfUpdate);
    assert_eq!(discarded.reason, "coordinator rejected the commit");
    assert!(alice.pending_control().expect("pending").is_none());
    assert_eq!(alice.epoch().expect("epoch"), 1);
    assert_eq!(
        store
            .load_mls_group_state(&group_key)
            .expect("load")
            .expect("present")
            .epoch,
        1,
        "the discarded pending branch is replaced by the committed snapshot"
    );
    let rejected_digest = pending.commit_digest;
    let rejected_commit = pending.commit.clone();

    // 旧 epoch 仍可继续加解密（两个方向）。
    let message = alice.encrypt("回滚后的旧 epoch").expect("encrypt");
    assert_eq!(bob.decrypt(&message, "Alice").expect("decrypt"), "回滚后的旧 epoch");
    let reply = bob.encrypt("旧 epoch 回复").expect("encrypt");
    assert_eq!(alice.decrypt(&reply, "Bob").expect("decrypt"), "旧 epoch 回复");

    // 重开后是回滚后的完整状态：无 pending、epoch 仍为 1。
    drop(store);
    drop(alice);
    let mut store = fixture.open("alice");
    let mut alice = MlsDevice::import_from_store(&store, &group_key).expect("import after discard");
    assert_eq!(alice.epoch().expect("epoch"), 1);
    assert!(alice.pending_control().expect("pending").is_none());
    // 可以重新准备并合并新的控制提交。
    let pending = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut store)
        .expect("prepare again");
    // 重建的控制提交必须使用新的密钥材料：提交字节与摘要都不能复用被丢弃的候选。
    assert_ne!(pending.commit_digest, rejected_digest);
    assert_ne!(pending.commit, rejected_commit);
    alice
        .merge_accepted_pending(&pending.acceptance(), &mut store)
        .expect("merge again");
    assert_eq!(alice.epoch().expect("epoch"), 2);
}

#[test]
fn entry_validation_rejects_wrong_group_epoch_sender_and_tampering() {
    let fixture = Fixture::new("entries");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");

    // 准备一个 Alice 的提交用于后续条目测试（不合并，只取 commit 字节）。
    let mut alice_store = fixture.open("alice");
    alice.export_to_store(&mut alice_store).expect("export");
    let pending = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut alice_store)
        .expect("prepare self update");

    // 群不一致。
    assert_eq!(
        bob.process_verified_entry(&commit_entry(
            "group-other",
            1,
            "Alice",
            pending.commit.clone(),
            self_update_expectation(),
        ))
        .unwrap_err()
        .code(),
        "INVALID_CIPHERTEXT"
    );
    // epoch 落后（乱序/重复）。
    assert_eq!(
        bob.process_verified_entry(&commit_entry(
            &group_key,
            0,
            "Alice",
            pending.commit.clone(),
            self_update_expectation(),
        ))
        .unwrap_err()
        .code(),
        "STALE_EPOCH"
    );
    // epoch 超前（缺少中间条目）。
    assert_eq!(
        bob.process_verified_entry(&commit_entry(
            &group_key,
            3,
            "Alice",
            pending.commit.clone(),
            self_update_expectation(),
        ))
        .unwrap_err()
        .code(),
        "MISSING_LOG_ENTRIES"
    );
    // 篡改后的提交字节：握手认证失败会消耗该代密钥，因此在“从已提交快照恢复的干净设备”上
    // 验证拒绝与 fail-closed；原设备保持干净继续后续用例。
    let mut bob_store = fixture.open("bob");
    bob.export_to_store(&mut bob_store).expect("export bob");
    let mut tampered = pending.commit.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    {
        let mut clean =
            MlsDevice::import_from_store(&bob_store, &group_key).expect("import clean bob");
        assert_eq!(
            clean
                .process_verified_entry(&commit_entry(
                    &group_key,
                    1,
                    "Alice",
                    tampered,
                    self_update_expectation(),
                ))
                .unwrap_err()
                .code(),
            "INVALID_CIPHERTEXT"
        );
        assert!(
            clean.epoch().is_err(),
            "a failed handshake must stop the group branch (fail-closed)"
        );
        // 从已提交快照恢复即可继续（不假设失败无副作用）。
        let recovered = MlsDevice::import_from_store(&bob_store, &group_key)
            .expect("recover from committed snapshot");
        assert_eq!(recovered.epoch().expect("epoch"), 1);
    }

    // 应用消息：来源不符必须拒绝（解密成功后仍要核对已认证作者）。
    let message = alice.encrypt("来源校验").expect("encrypt");
    assert_eq!(
        bob.process_verified_entry(&app_entry(&group_key, 1, "Mallory", message.clone()))
            .unwrap_err()
            .code(),
        "INVALID_CIPHERTEXT"
    );
    // 同一条密文已被消费（错误来源尝试已消耗发送方 ratchet），再次处理必须失败。
    let fresh = alice.encrypt("来源校验 2").expect("encrypt");
    let processed = bob
        .process_verified_entry(&app_entry(&group_key, 1, "Alice", fresh.clone()))
        .expect("process application");
    match processed {
        ProcessedEntry::Application {
            sender_identity,
            epoch,
            plaintext,
        } => {
            assert_eq!(sender_identity, "Alice");
            assert_eq!(epoch, 1);
            assert_eq!(plaintext, "来源校验 2".as_bytes().to_vec());
        }
        other => panic!("expected application, got {other:?}"),
    }
    // 重复的应用消息（重放）必须显式拒绝。
    assert_eq!(
        bob.process_verified_entry(&app_entry(&group_key, 1, "Alice", fresh))
            .unwrap_err()
            .code(),
        "INVALID_CIPHERTEXT"
    );

    // Bob 追上 epoch 2 后：
    bob.process_verified_entry(&commit_entry(
        &group_key,
        1,
        "Alice",
        pending.commit.clone(),
        self_update_expectation(),
    ))
    .expect("process commit");
    assert_eq!(bob.epoch().expect("epoch"), 2);
    // 重复提交（epoch 落后）显式拒绝。
    assert_eq!(
        bob.process_verified_entry(&commit_entry(
            &group_key,
            1,
            "Alice",
            pending.commit.clone(),
            self_update_expectation(),
        ))
        .unwrap_err()
        .code(),
        "STALE_EPOCH"
    );
    // 条目声明当前 epoch，但 MLS 消息本身仍是旧 epoch：显式拒绝，不猜测。
    assert_eq!(
        bob.process_verified_entry(&commit_entry(
            &group_key,
            2,
            "Alice",
            pending.commit.clone(),
            self_update_expectation(),
        ))
        .unwrap_err()
        .code(),
        "INVALID_CIPHERTEXT"
    );
}

#[test]
fn entry_commit_is_rejected_while_a_local_control_commit_is_pending() {
    let fixture = Fixture::new("pending-conflict");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let mut alice_store = fixture.open("alice");
    let mut bob_store = fixture.open("bob");
    alice.export_to_store(&mut alice_store).expect("export alice");
    bob.export_to_store(&mut bob_store).expect("export bob");

    let alice_pending = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut alice_store)
        .expect("alice prepare");
    let bob_pending = bob
        .prepare_control(ControlOperation::SelfUpdate, &mut bob_store)
        .expect("bob prepare");

    // Bob 有本地 pending 时不得处理对端竞争 Commit（先显式解决本分支）。
    assert_eq!(
        bob.process_verified_entry(&commit_entry(
            &group_key,
            1,
            "Alice",
            alice_pending.commit.clone(),
            self_update_expectation(),
        ))
        .unwrap_err()
        .code(),
        "PENDING_MISMATCH"
    );
    assert!(bob.pending_control().expect("pending").is_some());

    // 丢弃本机 pending 后可以正常处理对端 Commit。
    bob.discard_rejected_pending(
        &bob_pending.rejection("lost the control head race"),
        &mut bob_store,
    )
    .expect("discard bob pending");
    let processed = bob
        .process_verified_entry(&commit_entry(
            &group_key,
            1,
            "Alice",
            alice_pending.commit.clone(),
            self_update_expectation(),
        ))
        .expect("process competing commit after discarding");
    match processed {
        ProcessedEntry::Commit { new_epoch, .. } => assert_eq!(new_epoch, 2),
        other => panic!("expected commit, got {other:?}"),
    }
}

#[test]
fn control_result_mismatch_stops_the_group_branch() {
    let fixture = Fixture::new("result-mismatch");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let mut alice_store = fixture.open("alice");
    let mut bob_store = fixture.open("bob");
    alice.export_to_store(&mut alice_store).expect("export alice");
    bob.export_to_store(&mut bob_store).expect("export bob");
    let pending = alice
        .prepare_control(ControlOperation::SelfUpdate, &mut alice_store)
        .expect("prepare self update");

    // 预期成员变化与实际不一致：拒绝合并并停止该群分支（fail-closed）。
    let error = bob
        .process_verified_entry(&commit_entry(
            &group_key,
            1,
            "Alice",
            pending.commit.clone(),
            CommitExpectation {
                added: 0,
                removed: 1,
                updated: 0,
            },
        ))
        .unwrap_err();
    assert_eq!(error_code(&error), "INVALID_CONTROL_RESULT");
    assert!(
        bob.epoch().is_err(),
        "the group branch must be stopped, not silently advanced"
    );
    assert_eq!(
        bob.pending_control().unwrap_err().code(),
        "CRYPTO_FAILURE",
        "a fail-closed device reports the group as not initialized"
    );

    // 从已提交的正式存储恢复（epoch 1），再用正确预期处理同一 Commit。
    let bob = MlsDevice::import_from_store(&bob_store, &group_key).expect("re-import bob");
    let mut bob = bob;
    assert_eq!(bob.epoch().expect("epoch"), 1);
    let processed = bob
        .process_verified_entry(&commit_entry(
            &group_key,
            1,
            "Alice",
            pending.commit.clone(),
            self_update_expectation(),
        ))
        .expect("process with matching expectation");
    match processed {
        ProcessedEntry::Commit { new_epoch, .. } => assert_eq!(new_epoch, 2),
        other => panic!("expected commit, got {other:?}"),
    }
    // 正式存储仍是 epoch 1（处理对端条目本身不写库，由 T3 事务负责）。
    assert_eq!(
        bob_store
            .load_mls_group_state(&group_key)
            .expect("load")
            .expect("present")
            .epoch,
        1
    );
}

#[test]
fn welcome_join_rejects_unauthorized_bindings_and_replays() {
    // 群不一致。
    {
        let fixture = Fixture::new("welcome-group");
        let (_alice, _bob, mut carol, pending, _alice_store, mut carol_store) =
            pending_add_carol(&fixture);
        let error = carol
            .join_from_welcome(
                pending.welcome.as_ref().expect("welcome"),
                &WelcomeAcceptance {
                    group_id: "group-other".into(),
                    expected_epoch: 2,
                    members: vec!["Alice".into(), "Bob".into(), "Carol".into()],
                },
                &mut carol_store,
            )
            .unwrap_err();
        assert_eq!(error_code(&error), "WELCOME_NOT_AUTHORIZED");
    }
    // 成员集合与已验证绑定不一致（漏掉 Carol）。
    {
        let fixture = Fixture::new("welcome-members");
        let (_alice, _bob, mut carol, pending, _alice_store, mut carol_store) =
            pending_add_carol(&fixture);
        let error = carol
            .join_from_welcome(
                pending.welcome.as_ref().expect("welcome"),
                &WelcomeAcceptance {
                    group_id: _alice.group_key().expect("group key"),
                    expected_epoch: 2,
                    members: vec!["Alice".into(), "Bob".into()],
                },
                &mut carol_store,
            )
            .unwrap_err();
        assert_eq!(error_code(&error), "WELCOME_NOT_AUTHORIZED");
    }
    // epoch 绑定不一致。
    {
        let fixture = Fixture::new("welcome-epoch");
        let (alice, _bob, mut carol, pending, _alice_store, mut carol_store) =
            pending_add_carol(&fixture);
        let error = carol
            .join_from_welcome(
                pending.welcome.as_ref().expect("welcome"),
                &WelcomeAcceptance {
                    group_id: alice.group_key().expect("group key"),
                    expected_epoch: 3,
                    members: vec!["Alice".into(), "Bob".into(), "Carol".into()],
                },
                &mut carol_store,
            )
            .unwrap_err();
        assert_eq!(error_code(&error), "WELCOME_NOT_AUTHORIZED");
    }
    // Welcome 属于另一台设备：不能用于本机。
    {
        let fixture = Fixture::new("welcome-foreign");
        let (alice, _bob, carol, pending, _alice_store, _carol_store) = pending_add_carol(&fixture);
        let mut stranger = MlsDevice::new("Stranger").expect("stranger");
        let mut stranger_store = fixture.open("stranger");
        let error = stranger
            .join_from_welcome(
                pending.welcome.as_ref().expect("welcome"),
                &WelcomeAcceptance {
                    group_id: alice.group_key().expect("group key"),
                    expected_epoch: 2,
                    members: vec!["Alice".into(), "Bob".into(), "Carol".into()],
                },
                &mut stranger_store,
            )
            .unwrap_err();
        assert_eq!(error_code(&error), "WELCOME_NOT_AUTHORIZED");
        drop(carol);
    }
    // 成功加入后再次用 Welcome 加入必须显式拒绝。
    {
        let fixture = Fixture::new("welcome-replay");
        let (_alice, _bob, mut carol, pending, _alice_store, mut carol_store) =
            pending_add_carol(&fixture);
        let group_key = _alice.group_key().expect("group key");
        let acceptance = WelcomeAcceptance {
            group_id: group_key.clone(),
            expected_epoch: 2,
            members: vec!["Alice".into(), "Bob".into(), "Carol".into()],
        };
        carol
            .join_from_welcome(
                pending.welcome.as_ref().expect("welcome"),
                &acceptance,
                &mut carol_store,
            )
            .expect("first join");
        let error = carol
            .join_from_welcome(
                pending.welcome.as_ref().expect("welcome"),
                &acceptance,
                &mut carol_store,
            )
            .unwrap_err();
        assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");
    }
}

#[test]
fn pending_add_with_welcome_survives_reopen() {
    let fixture = Fixture::new("reopen-add");
    let (alice, _bob, mut carol, pending, alice_store, mut carol_store) =
        pending_add_carol(&fixture);
    let group_key = alice.group_key().expect("group key");
    assert!(pending.welcome.is_some());
    assert_eq!(
        alice_store
            .load_mls_group_state(&group_key)
            .expect("load")
            .expect("present")
            .epoch,
        1
    );
    drop(alice_store);
    drop(alice);

    // 重开后 pending 的 Commit + Welcome 元数据完整保留，可以继续投递 Welcome 并合并。
    let mut store = fixture.open("alice");
    let mut alice = MlsDevice::import_from_store(&store, &group_key).expect("import pending add");
    let restored = alice
        .pending_control()
        .expect("pending")
        .expect("pending add survives reopen");
    assert_eq!(restored.operation, ControlOperationKind::AddMembers);
    assert_eq!(restored.welcome.as_ref(), pending.welcome.as_ref());
    assert_eq!(restored.commit_digest, pending.commit_digest);
    let welcome = restored.welcome.clone().expect("welcome");
    let joined = carol
        .join_from_welcome(
            &welcome,
            &WelcomeAcceptance {
                group_id: group_key.clone(),
                expected_epoch: 2,
                members: vec!["Alice".into(), "Bob".into(), "Carol".into()],
            },
            &mut carol_store,
        )
        .expect("carol joins after reopen");
    assert_eq!(joined.epoch, 2);
    let merged = alice
        .merge_accepted_pending(&restored.acceptance(), &mut store)
        .expect("merge after reopen");
    assert_eq!(merged.new_epoch, 2);
    assert_eq!(
        store
            .load_mls_group_state(&group_key)
            .expect("load")
            .expect("present")
            .epoch,
        2
    );
    // 重启/重开不会为同一群产生第二行状态。
    let group_rows: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM mls_group_state WHERE group_id = ?1",
            [&group_key],
            |row| row.get(0),
        )
        .expect("count group rows");
    assert_eq!(group_rows, 1, "reopening must not duplicate the MLS group");
}

#[test]
fn reopened_pending_can_be_discarded() {
    let fixture = Fixture::new("reopen-discard");
    let (mut alice, mut bob) = pair();
    let group_key = alice.group_key().expect("group key");
    let pending;
    {
        let mut store = fixture.open("alice");
        alice.export_to_store(&mut store).expect("export epoch 1");
        pending = alice
            .prepare_control(ControlOperation::SelfUpdate, &mut store)
            .expect("prepare self update");
    }
    drop(alice);
    // 重开后丢弃该 pending，仍回到 epoch 1 并写回正式库。
    let mut store = fixture.open("alice");
    let mut alice = MlsDevice::import_from_store(&store, &group_key).expect("import pending");
    assert_eq!(alice.epoch().expect("epoch"), 1);
    let restored = alice.pending_control().expect("pending").expect("present");
    assert_eq!(restored.commit_digest, pending.commit_digest);
    let discarded = alice
        .discard_rejected_pending(&restored.rejection("rejected after reopen"), &mut store)
        .expect("discard after reopen");
    assert_eq!(discarded.epoch, 1);
    assert!(alice.pending_control().expect("pending").is_none());
    assert_eq!(
        store
            .load_mls_group_state(&group_key)
            .expect("load")
            .expect("present")
            .epoch,
        1
    );
    let message = alice.encrypt("重开丢弃后继续").expect("encrypt");
    assert_eq!(bob.decrypt(&message, "Alice").expect("decrypt"), "重开丢弃后继续");
}

#[test]
fn remove_member_commit_applies_consistently_on_all_devices() {
    let fixture = Fixture::new("remove");
    let (mut alice, mut bob, mut carol, add_pending, mut alice_store, mut carol_store) =
        pending_add_carol(&fixture);
    let group_key = alice.group_key().expect("group key");
    let mut bob_store = fixture.open("bob");
    bob.export_to_store(&mut bob_store).expect("export bob");
    let acceptance = WelcomeAcceptance {
        group_id: group_key.clone(),
        expected_epoch: 2,
        members: vec!["Alice".into(), "Bob".into(), "Carol".into()],
    };
    carol
        .join_from_welcome(
            add_pending.welcome.as_ref().expect("welcome"),
            &acceptance,
            &mut carol_store,
        )
        .expect("carol joins");
    alice
        .merge_accepted_pending(&add_pending.acceptance(), &mut alice_store)
        .expect("merge add");
    bob.process_verified_entry(&commit_entry(
        &group_key,
        1,
        "Alice",
        add_pending.commit.clone(),
        CommitExpectation {
            added: 1,
            removed: 0,
            updated: 0,
        },
    ))
    .expect("bob applies add");
    assert_eq!(carol.epoch().expect("epoch"), 2);

    // 移除 Bob。
    let remove_pending = alice
        .prepare_control(
            ControlOperation::RemoveMembers {
                members: vec!["Bob".into()],
            },
            &mut alice_store,
        )
        .expect("prepare remove");
    assert_eq!(remove_pending.base_epoch, 2);
    assert_eq!(remove_pending.target_epoch, 3);
    assert_eq!(remove_pending.removed, 1);
    assert!(remove_pending.welcome.is_none());

    // Carol 处理提交后到 epoch 3。
    let processed = carol
        .process_verified_entry(&commit_entry(
            &group_key,
            2,
            "Alice",
            remove_pending.commit.clone(),
            CommitExpectation {
                added: 0,
                removed: 1,
                updated: 0,
            },
        ))
        .expect("carol applies remove");
    match processed {
        ProcessedEntry::Commit {
            new_epoch,
            removed,
            self_removed,
            ..
        } => {
            assert_eq!(new_epoch, 3);
            assert_eq!(removed, 1);
            assert!(!self_removed);
        }
        other => panic!("expected commit, got {other:?}"),
    }

    // Bob 处理“移除自己”的提交：epoch 前进且被标记 self_removed。
    let processed = bob
        .process_verified_entry(&commit_entry(
            &group_key,
            2,
            "Alice",
            remove_pending.commit.clone(),
            CommitExpectation {
                added: 0,
                removed: 1,
                updated: 0,
            },
        ))
        .expect("bob applies self removal");
    match processed {
        ProcessedEntry::Commit {
            new_epoch,
            self_removed,
            ..
        } => {
            assert_eq!(new_epoch, 3);
            assert!(self_removed, "bob must see that the commit removed him");
        }
        other => panic!("expected commit, got {other:?}"),
    }

    // Alice 合并后到 epoch 3；剩余成员继续通信，被移除设备不能解密。
    alice
        .merge_accepted_pending(&remove_pending.acceptance(), &mut alice_store)
        .expect("merge remove");
    assert_eq!(alice.epoch().expect("epoch"), 3);
    let message = alice.encrypt("只剩 Alice 与 Carol").expect("encrypt");
    assert_eq!(
        carol.decrypt(&message, "Alice").expect("carol decrypt"),
        "只剩 Alice 与 Carol"
    );
    assert!(bob.decrypt(&message, "Alice").is_err(), "removed device must be evicted");
    assert!(bob.encrypt("被移除后发言").is_err());
}

#[test]
fn key_package_and_member_inputs_are_validated() {
    let fixture = Fixture::new("inputs");
    let (mut alice, _bob) = pair();
    let mut store = fixture.open("alice");
    alice.export_to_store(&mut store).expect("export");

    // 空 Add / 空 Remove / 未知成员 / 重复移除 / 移除自己 都必须显式拒绝。
    assert_eq!(
        alice
            .prepare_control(ControlOperation::AddMembers { key_packages: vec![] }, &mut store)
            .unwrap_err()
            .code(),
        "INVALID_STATE_TRANSITION"
    );
    assert_eq!(
        alice
            .prepare_control(
                ControlOperation::RemoveMembers { members: vec![] },
                &mut store
            )
            .unwrap_err()
            .code(),
        "INVALID_STATE_TRANSITION"
    );
    assert_eq!(
        alice
            .prepare_control(
                ControlOperation::RemoveMembers {
                    members: vec!["Mallory".into()],
                },
                &mut store
            )
            .unwrap_err()
            .code(),
        "INVALID_STATE_TRANSITION"
    );
    assert_eq!(
        alice
            .prepare_control(
                ControlOperation::RemoveMembers {
                    members: vec!["Alice".into()],
                },
                &mut store
            )
            .unwrap_err()
            .code(),
        "INVALID_STATE_TRANSITION"
    );
    // 畸形 KeyPackage。
    assert_eq!(
        alice
            .prepare_control(
                ControlOperation::AddMembers {
                    key_packages: vec![vec![1, 2, 3]],
                },
                &mut store
            )
            .unwrap_err()
            .code(),
        "INVALID_KEY_PACKAGE"
    );
    // 已经是成员的 KeyPackage 不能再加一次。
    let mut bob = MlsDevice::new("Bob2").expect("bob2");
    let mut bob_store = fixture.open("bob2");
    let bob_package = bob.key_package().expect("key package");
    // 先把 Bob2 加进群，再尝试重复添加同一包。
    let pending = alice
        .prepare_control(
            ControlOperation::AddMembers {
                key_packages: vec![bob_package.clone()],
            },
            &mut store,
        )
        .expect("add bob2");
    bob.join_from_welcome(
        pending.welcome.as_ref().expect("welcome"),
        &WelcomeAcceptance {
            group_id: alice.group_key().expect("group key"),
            expected_epoch: 2,
            members: vec!["Alice".into(), "Bob".into(), "Bob2".into()],
        },
        &mut bob_store,
    )
    .expect("bob2 joins");
    alice
        .merge_accepted_pending(&pending.acceptance(), &mut store)
        .expect("merge");
    // 重复添加已存在的签名公钥必须拒绝（即使 KeyPackage 本身合法）。
    let error = alice
        .prepare_control(
            ControlOperation::AddMembers {
                key_packages: vec![bob_package],
            },
            &mut store,
        )
        .unwrap_err();
    assert_eq!(error_code(&error), "INVALID_STATE_TRANSITION");
    // 无 pending 时 merge/discard 显式失败。
    assert_eq!(
        alice
            .merge_accepted_pending(
                &ControlAcceptance {
                    group_id: alice.group_key().expect("group key"),
                    base_epoch: 2,
                    target_epoch: 3,
                    commit_digest: [0u8; 32],
                },
                &mut store
            )
            .unwrap_err()
            .code(),
        "PENDING_MISMATCH"
    );
    assert_eq!(
        alice
            .discard_rejected_pending(
                &ControlRejection {
                    group_id: alice.group_key().expect("group key"),
                    base_epoch: 2,
                    commit_digest: [0u8; 32],
                    reason: "no pending",
                },
                &mut store
            )
            .unwrap_err()
            .code(),
        "PENDING_MISMATCH"
    );
}
