//! CTRLREV 支撑：`MlsDevice::stage_self_update_commit` 的验收。
//!
//! 覆盖：staging 后 epoch 不变、pending 存在、返回字节可被另一设备应用并推进 epoch、
//! 重复 staging 显式拒绝（已有 pending）。
use chat_crypto::control::{CommitExpectation, WelcomeAcceptance};
use chat_crypto::engine::MlsDevice;

const EXPECTATION: CommitExpectation = CommitExpectation {
    added: 0,
    removed: 0,
    updated: 0,
};

fn pair() -> (MlsDevice, MlsDevice) {
    let mut alice = MlsDevice::new("alice").expect("alice");
    let mut bob = MlsDevice::new("bob").expect("bob");
    let welcome = alice
        .create_and_invite(&bob.key_package().expect("bob key package"), &bob)
        .expect("create and invite");
    bob.join(&welcome, &alice).expect("bob joins");
    (alice, bob)
}

#[test]
fn staging_a_self_update_leaves_the_epoch_unchanged_and_keeps_pending() {
    let (mut alice, _bob) = pair();
    let before = alice.epoch().expect("epoch before");
    assert_eq!(before, 1, "bootstrap commit is already merged");
    let commit = alice.stage_self_update_commit().expect("stage commit");
    assert!(!commit.is_empty(), "the caller gets deliverable bytes");
    assert_eq!(
        alice.epoch().expect("epoch after staging"),
        before,
        "staging must not merge or advance the epoch"
    );
    // pending 存在 → 重复 staging 显式拒绝（不覆盖、不叠加）。
    let error = alice
        .stage_self_update_commit()
        .expect_err("a second staging must be rejected");
    assert!(
        error.to_string().contains("pending commit"),
        "unexpected error: {error}"
    );
    assert_eq!(alice.epoch().expect("epoch stays"), before);
}

#[test]
fn the_staged_bytes_advance_a_peer_epoch_once() {
    let (mut alice, mut bob) = pair();
    let alice_epoch = alice.epoch().expect("alice epoch");
    let bob_epoch = bob.epoch().expect("bob epoch");
    assert_eq!(alice_epoch, bob_epoch);
    let commit = alice.stage_self_update_commit().expect("stage commit");
    // 对端处理该提交：epoch 推进 1，作者身份按 MLS 认证结果给出。
    let entry = chat_crypto::control::VerifiedGroupEntry {
        group_id: bob.group_key().expect("bob group key"),
        epoch: bob_epoch,
        sender_identity: "alice".to_owned(),
        expectation: Some(EXPECTATION),
        payload: commit,
    };
    let processed = bob
        .process_verified_entry(&entry)
        .expect("bob processes the staged commit");
    match processed {
        chat_crypto::control::ProcessedEntry::Commit {
            old_epoch,
            new_epoch,
            added,
            removed,
            ..
        } => {
            assert_eq!(old_epoch, bob_epoch);
            assert_eq!(new_epoch, bob_epoch + 1);
            assert_eq!(added, 0, "a self update adds no members");
            assert_eq!(removed, 0, "a self update removes no members");
        }
        other => panic!("expected a merged commit, got {other:?}"),
    }
    assert_eq!(bob.epoch().expect("bob epoch after"), bob_epoch + 1);
    // 本机仍然停在旧 epoch（staging 没有合并）——这是有意的 T2 语义。
    assert_eq!(alice.epoch().expect("alice epoch stays"), alice_epoch);
}

#[test]
fn staging_requires_an_initialized_group() {
    let mut lonely = MlsDevice::new("nobody").expect("device");
    let error = lonely
        .stage_self_update_commit()
        .expect_err("no group, no commit");
    assert!(
        error.to_string().to_lowercase().contains("not initialized")
        || error.to_string().to_lowercase().contains("group"),
        "unexpected error: {error}"
    );
}

#[allow(dead_code)]
fn _unused(_: WelcomeAcceptance) {}

