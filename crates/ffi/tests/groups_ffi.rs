//! 群治理 FFI 端到端用例：经 GroupFfi（真 OpenMLS + 临时正式加密库）走完整治理链路。
//!
//! 覆盖：建群 → 快照 → 改名 → 禁言 → 移除 → 接受/丢弃；错误码冻结且失败不留半状态；
//! 本地凭据留空时使用持久身份的设备 ID；关闭后 `CORE_CLOSED`。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::groups_ffi::{GroupFfi, GroupMemberInputFfi, GroupViewFfi};
use chat_ffi::identity_ffi::IdentityFfi;
use chat_ffi::local_store::FfiError;

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const NODE: &str = "node:1:1111111111111111111111111111111111111111111111111111111111111111";

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
            "chat-ffi-groups-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
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

fn open(dir: &FixtureDir) -> Arc<GroupFfi> {
    GroupFfi::open(dir.database(), master_key_hex(), None).expect("open group ffi")
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

fn credentials(view: &GroupViewFfi) -> Vec<String> {
    view.members
        .iter()
        .map(|member| member.credential.clone())
        .collect()
}

fn member(view: &GroupViewFfi, credential: &str) -> chat_ffi::groups_ffi::GroupMemberFfi {
    view.members
        .iter()
        .find(|member| member.credential == credential)
        .cloned()
        .expect("member exists")
}

#[test]
fn create_snapshot_rename_mute_and_remove_through_ffi() {
    let dir = FixtureDir::new();
    let groups = open(&dir);

    let created = groups
        .group_create(
            "FFI 群".to_owned(),
            "alice".to_owned(),
            "bob".to_owned(),
            Vec::new(),
        )
        .expect("create group through ffi");
    assert!(created.group_id.starts_with("grp:1:"));
    assert_eq!(created.name, "FFI 群");
    assert_eq!(created.epoch, 1);
    assert_eq!(created.policy_version, 1);
    assert_eq!(created.role, "owner");
    assert_eq!(created.control_head, "0".repeat(64));
    assert_eq!(credentials(&created), vec!["alice", "bob"]);
    assert_eq!(member(&created, "alice").role, "owner");
    assert_eq!(member(&created, "bob").role, "member");
    assert!(created.pending.is_none());
    let group_id = created.group_id.clone();

    // 快照与建群结果一致。
    let snapshot = groups.group_snapshot(group_id.clone()).expect("snapshot");
    assert_eq!(snapshot, created);

    // 改名：策略版本 +1，并准备真实 MLS 自更新提交（pending，epoch 不推进）。
    let renamed = groups
        .group_rename(group_id.clone(), "alice".to_owned(), "FFI 改名".to_owned())
        .expect("rename");
    assert_eq!(renamed.name, "FFI 改名");
    assert_eq!(renamed.policy_version, 2);
    assert_eq!(renamed.epoch, 1, "preparing the commit does not advance the epoch");
    let rename_pending = renamed.pending.as_ref().expect("rename prepares a commit");
    assert_eq!(rename_pending.base_epoch, 1);
    assert_eq!(rename_pending.target_epoch, 2);
    // 接受后 epoch +1、名字保持、pending 清空。
    let after_rename = groups
        .group_accept_pending(group_id.clone())
        .expect("accept the rename commit");
    assert_eq!(after_rename.epoch, 2);
    assert_eq!(after_rename.name, "FFI 改名");
    assert!(after_rename.pending.is_none());
    // 同名幂等：版本与状态版本不再推进（此时没有 pending）。
    let same = groups
        .group_rename(group_id.clone(), "alice".to_owned(), "FFI 改名".to_owned())
        .expect("same-name rename");
    assert_eq!(same.policy_version, 2);
    assert_eq!(same.state_version, after_rename.state_version);
    assert!(same.pending.is_none());

    // 禁言：策略层 + 自更新提交（同一条提交路径），布尔往返。
    let muted = groups
        .group_set_muted(group_id.clone(), "alice".to_owned(), "bob".to_owned(), true)
        .expect("mute");
    assert!(member(&muted, "bob").muted);
    assert_eq!(muted.policy_version, 3);
    assert!(muted.pending.is_some(), "mute prepares a commit");
    let after_mute = groups
        .group_accept_pending(group_id.clone())
        .expect("accept the mute commit");
    assert_eq!(after_mute.epoch, 3);
    assert!(member(&after_mute, "bob").muted, "muted survives the acceptance");
    // 移除：真实 MLS 移除提交，进入 pending，目标从正式成员消失。
    let removed = groups
        .group_remove_member(group_id.clone(), "alice".to_owned(), "bob".to_owned())
        .expect("remove member");
    assert_eq!(credentials(&removed), vec!["alice"]);
    assert_eq!(
        removed
            .pending_removed
            .iter()
            .map(|member| member.credential.clone())
            .collect::<Vec<_>>(),
        vec!["bob"]
    );
    let pending = removed.pending.as_ref().expect("pending removal commit");
    assert_eq!(pending.operation, "remove_members");
    assert_eq!(pending.removed, 1);
    assert_eq!(pending.base_epoch, 3);
    assert_eq!(pending.target_epoch, 4);

    // 接受：epoch 前进、pending 清空、被移除者彻底消失。
    let accepted = groups
        .group_accept_pending(group_id.clone())
        .expect("accept pending");
    assert_eq!(accepted.epoch, 4);
    assert!(accepted.pending.is_none());
    assert!(accepted.pending_removed.is_empty());
    // 策略版本：建群 1 → 改名 2 → 禁言 3 → 移除 4；接受只是确认，不改版本。
    assert_eq!(removed.policy_version, 4);
    assert_eq!(accepted.policy_version, 4, "accept does not change policy version");
    assert_eq!(credentials(&accepted), vec!["alice"]);
    let final_snapshot = groups.group_snapshot(group_id).expect("final snapshot");
    assert_eq!(final_snapshot, accepted);
}

#[test]
fn role_change_accept_and_discard_through_ffi() {
    let dir = FixtureDir::new();
    let groups = open(&dir);
    let created = groups
        .group_create(
            "治理 FFI".to_owned(),
            "alice".to_owned(),
            "bob".to_owned(),
            Vec::new(),
        )
        .expect("create");
    let group_id = created.group_id.clone();

    // 改角色：pending self_update。
    let promoted = groups
        .group_set_role(
            group_id.clone(),
            "alice".to_owned(),
            "bob".to_owned(),
            "admin".to_owned(),
        )
        .expect("set role");
    assert_eq!(member(&promoted, "bob").role, "admin");
    assert_eq!(promoted.policy_version, 2);
    assert_eq!(
        promoted.pending.as_ref().expect("pending").operation,
        "self_update"
    );

    // 丢弃：epoch 不变、pending 清空、策略版本不回退、审计记录原因码。
    let discarded = groups
        .group_discard_pending(group_id.clone(), "node_rejected".to_owned())
        .expect("discard");
    assert_eq!(discarded.epoch, 1);
    assert!(discarded.pending.is_none());
    assert_eq!(discarded.policy_version, 2);
    let cleared = discarded
        .last_control_cleared
        .as_ref()
        .expect("clear note");
    assert_eq!(cleared.operation, "self_update");
    assert_eq!(cleared.reason_code, "node_rejected");
    assert_eq!(cleared.base_epoch, 1);
    assert_eq!(cleared.target_epoch, 2);
    assert_eq!(member(&discarded, "bob").role, "admin", "policy effect stays");

    // 丢弃后仍可继续治理：改回 member 并接受。
    let demoted = groups
        .group_set_role(
            group_id.clone(),
            "alice".to_owned(),
            "bob".to_owned(),
            "member".to_owned(),
        )
        .expect("set role again");
    assert_eq!(demoted.policy_version, 3);
    assert_eq!(
        demoted.pending.as_ref().expect("pending").base_epoch,
        1,
        "still based on epoch 1 after the discard"
    );
    let accepted = groups
        .group_accept_pending(group_id.clone())
        .expect("accept");
    assert_eq!(accepted.epoch, 2);
    assert!(accepted.pending.is_none());
    assert_eq!(member(&accepted, "bob").role, "member");
}

#[test]
fn error_codes_are_frozen_and_failures_leave_no_state() {
    let dir = FixtureDir::new();
    let groups = open(&dir);
    let created = groups
        .group_create(
            "错误码".to_owned(),
            "alice".to_owned(),
            "bob".to_owned(),
            Vec::new(),
        )
        .expect("create");
    let group_id = created.group_id.clone();
    let unknown = "grp:1:0000000000000000000000000000000000000000000000000000000000000009";

    // 未知群 / 畸形群 ID。
    assert_eq!(
        code(groups.group_snapshot(unknown.to_owned()).unwrap_err()),
        "INVALID_TARGET"
    );
    assert_eq!(
        code(groups.group_snapshot("not-a-group".to_owned()).unwrap_err()),
        "INVALID_ID"
    );
    // 非法角色文本 / 非法名称（FFI 边界）。
    assert_eq!(
        code(
            groups
                .group_set_role(
                    group_id.clone(),
                    "alice".to_owned(),
                    "bob".to_owned(),
                    "superuser".to_owned()
                )
                .unwrap_err()
        ),
        "INVALID_INPUT"
    );
    assert_eq!(
        code(
            groups
                .group_rename(group_id.clone(), "alice".to_owned(), "x".repeat(65))
                .unwrap_err()
        ),
        "INVALID_INPUT"
    );
    // 权限：普通成员不能改名 / 不能改角色 / 不能移除别人。
    assert_eq!(
        code(
            groups
                .group_rename(group_id.clone(), "bob".to_owned(), "bob 改名".to_owned())
                .unwrap_err()
        ),
        "PERMISSION_DENIED"
    );
    assert_eq!(
        code(
            groups
                .group_set_role(
                    group_id.clone(),
                    "bob".to_owned(),
                    "bob".to_owned(),
                    "admin".to_owned()
                )
                .unwrap_err()
        ),
        "PERMISSION_DENIED"
    );
    assert_eq!(
        code(
            groups
                .group_remove_member(group_id.clone(), "alice".to_owned(), "alice".to_owned())
                .unwrap_err()
        ),
        "PERMISSION_DENIED"
    );
    // 未知目标。
    assert_eq!(
        code(
            groups
                .group_remove_member(group_id.clone(), "alice".to_owned(), "carol".to_owned())
                .unwrap_err()
        ),
        "INVALID_TARGET"
    );
    // 没有 pending 时接受 / 丢弃。
    assert_eq!(
        code(groups.group_accept_pending(group_id.clone()).unwrap_err()),
        "PENDING_MISMATCH"
    );
    assert_eq!(
        code(
            groups
                .group_discard_pending(group_id.clone(), "node_rejected".to_owned())
                .unwrap_err()
        ),
        "PENDING_MISMATCH"
    );
    assert_eq!(
        code(
            groups
                .group_discard_pending(group_id.clone(), String::new())
                .unwrap_err()
        ),
        "INVALID_INPUT"
    );

    // 所有失败都没有半状态：快照与建群时完全一致。
    let snapshot = groups.group_snapshot(group_id).expect("snapshot");
    assert_eq!(snapshot, created);
}

#[test]
fn empty_local_credential_uses_the_persisted_identity_device() {
    let dir = FixtureDir::new();
    // 先在同一个库里建立持久身份（IdentityFfi 独占写者，用完立即释放）。
    let identity = IdentityFfi::open_persisted(
        dir.database(),
        master_key_hex(),
        NODE.to_owned(),
    )
    .expect("persisted identity");
    let device_id = identity.account().device_id;
    identity.shutdown();
    drop(identity);

    let groups = open(&dir);
    let created = groups
        .group_create(
            "身份派生凭据".to_owned(),
            String::new(), // 空 → 使用持久身份的设备 ID
            "bob".to_owned(),
            Vec::new(),
        )
        .expect("create with persisted identity credential");
    assert_eq!(created.members[0].credential, device_id);
    assert_eq!(created.members[0].role, "owner");
    assert!(created.members[0].is_local);
}

#[test]
fn create_without_identity_and_empty_credential_fails() {
    let dir = FixtureDir::new();
    let groups = open(&dir);
    let error = groups
        .group_create(
            "无身份".to_owned(),
            String::new(),
            "bob".to_owned(),
            Vec::new(),
        )
        .expect_err("no persisted identity");
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");
    // 对象仍可用：显式凭据就能建群。
    let created = groups
        .group_create(
            "显式凭据".to_owned(),
            "alice".to_owned(),
            "bob".to_owned(),
            vec![GroupMemberInputFfi {
                credential: "carol".to_owned(),
                key_package: Vec::new(),
            }],
        );
    // 空 KeyPackage 由 groups 显式拒绝（INVALID_CANDIDATE）。
    assert_eq!(code(created.unwrap_err()), "INVALID_CANDIDATE");
}

#[test]
fn closed_object_reports_core_closed() {
    let dir = FixtureDir::new();
    let groups = open(&dir);
    let created = groups
        .group_create(
            "关闭".to_owned(),
            "alice".to_owned(),
            "bob".to_owned(),
            Vec::new(),
        )
        .expect("create");
    assert_eq!(groups.node(), None);
    groups.shutdown();
    assert!(!groups.is_open());
    assert_eq!(
        code(groups.group_snapshot(created.group_id.clone()).unwrap_err()),
        "CORE_CLOSED"
    );
    assert_eq!(
        code(
            groups
                .group_rename(created.group_id, "alice".to_owned(), "新名".to_owned())
                .unwrap_err()
        ),
        "CORE_CLOSED"
    );
}
