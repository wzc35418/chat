//! GROUPS-001/002：建群与群快照的端到端用例（真实 OpenMLS + 真临时加密库）。
//!
//! 覆盖：建群 → 快照一致；关闭重开后快照一致；重复成员/自我加法/超限/非法名称/非法请求
//! 在写入前被拒；未知群显式拒；MLS 级拒绝后不留 pending、不留快照（不留半状态）。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupMemberKeyPackage, MAX_GROUP_MEMBERS,
    MAX_GROUP_NAME_BYTES, create_group, group_snapshot,
};
use chat_core::send_runtime::protocol_group_id_for;
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
use chat_protocol::GroupId;
use chat_storage::formal::{
    KeyProvenance, MlsGroupStateInput, ProtectedStorageKey, Store, StoreOptions, open_store,
};

const MASTER_KEY: [u8; 32] = [0x5a; 32];

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
            "chat-core-groups-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open(dir: &FixtureDir) -> Store {
    open_store(
        StoreOptions::new(dir.database(), KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("open formal store")
}

fn member(device: &MlsDevice, credential: &str) -> GroupMemberKeyPackage {
    GroupMemberKeyPackage {
        credential: credential.to_owned(),
        key_package: device.key_package().expect("key package"),
    }
}

/// 建群夹具：alice 创建，bob 引导，carol/dave 为追加成员。
struct Fixture {
    dir: FixtureDir,
    store: Store,
    alice: MlsDevice,
    bob: MlsDevice,
    carol: MlsDevice,
    dave: MlsDevice,
}

impl Fixture {
    fn new() -> Self {
        let dir = FixtureDir::new();
        let store = open(&dir);
        Self {
            dir,
            store,
            alice: MlsDevice::new("alice").expect("alice"),
            bob: MlsDevice::new("bob").expect("bob"),
            carol: MlsDevice::new("carol").expect("carol"),
            dave: MlsDevice::new("dave").expect("dave"),
        }
    }

    fn create(&mut self, name: &str) -> chat_core::groups::GroupView {
        let request = CreateGroupRequest {
            name: name.to_owned(),
            local_credential: "alice".to_owned(),
            policy_version: chat_core::groups::GROUP_POLICY_VERSION,
            members: vec![member(&self.carol, "carol"), member(&self.dave, "dave")],
        };
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &self.bob,
        };
        create_group(&mut self.alice, &peer, &mut self.store, &request).expect("create group")
    }
}

fn assert_same_snapshot(left: &chat_core::groups::GroupView, right: &chat_core::groups::GroupView) {
    assert_eq!(left.group_id, right.group_id);
    assert_eq!(left.name, right.name);
    assert_eq!(left.epoch, right.epoch);
    assert_eq!(left.policy_version, right.policy_version);
    assert_eq!(left.role, right.role);
    assert_eq!(left.created_at_ms, right.created_at_ms);
    assert_eq!(left.mls_group_key, right.mls_group_key);
    assert_eq!(left.state_version, right.state_version);
    assert_eq!(left.members, right.members);
    assert_eq!(left.pending_members, right.pending_members);
    assert_eq!(left.pending, right.pending);
}

#[test]
fn create_group_then_snapshot_matches() {
    let mut fixture = Fixture::new();
    let view = fixture.create("研究小组");

    // 真实 MLS 群：epoch 1（引导提交已合并），协议群 ID 由 MLS 群键派生。
    assert_eq!(view.name, "研究小组");
    assert_eq!(view.epoch, 1, "bootstrap commit is merged locally");
    assert_eq!(view.policy_version, 1);
    assert_eq!(view.role, MemberRole::Owner);
    assert!(view.created_at_ms > 0);
    assert_eq!(view.group_id, protocol_group_id_for(&view.mls_group_key).unwrap());
    assert!(view.group_id.starts_with("grp:1:"));
    assert_eq!(view.state_version, 1);

    // 正式成员只有创建者与引导成员；追加成员仍是 pending（不混入正式成员）。
    assert_eq!(view.members.len(), 2);
    assert_eq!(view.members[0].credential, "alice");
    assert!(view.members[0].is_local);
    assert_eq!(view.members[0].role, MemberRole::Owner);
    assert_eq!(view.members[1].credential, "bob");
    assert_eq!(view.members[1].role, MemberRole::Member);
    assert_eq!(view.pending_members.len(), 2);
    assert_eq!(view.pending_members[0].credential, "carol");
    assert!(view.pending_members.iter().all(|member| member.pending));

    // pending 是真实 CRYPTO-005 控制提交（含 Commit + Welcome，added = 2）。
    let pending = view.pending.as_ref().expect("pending add members");
    assert_eq!(pending.operation, "add_members");
    assert_eq!(pending.base_epoch, 1);
    assert_eq!(pending.target_epoch, 2);
    assert_eq!(pending.added, 2);
    assert_eq!(pending.removed, 0);
    assert_eq!(pending.commit_digest.len(), 64);
    let device_pending = fixture
        .alice
        .pending_control()
        .expect("pending control")
        .expect("pending exists");
    assert_eq!(device_pending.added, 2);
    assert!(device_pending.welcome.is_some(), "add operation carries a Welcome");
    assert_eq!(device_pending.commit.len(), device_pending.commit.len());

    // 快照与建群结果一致（同一份真实状态）。
    let snapshot = group_snapshot(&fixture.store, &view.group_id).expect("snapshot");
    assert_same_snapshot(&view, &snapshot);
}

#[test]
fn snapshot_survives_reopen() {
    let mut fixture = Fixture::new();
    let view = fixture.create("重开后仍然一致");
    let group_id = view.group_id.clone();
    let database = fixture.dir.database();
    // 关闭库（释放独占写锁）后重新打开同一库文件。
    drop(fixture.store);
    let reopened = open_store(
        StoreOptions::new(&database, KeyProvenance::Raw32),
        ProtectedStorageKey::from_bytes(MASTER_KEY),
    )
    .expect("reopen formal store");
    let snapshot = group_snapshot(&reopened, &group_id).expect("snapshot after reopen");
    assert_eq!(snapshot.name, view.name);
    assert_eq!(snapshot.epoch, view.epoch);
    assert_eq!(snapshot.mls_group_key, view.mls_group_key);
    assert_eq!(snapshot.created_at_ms, view.created_at_ms);
    assert_eq!(snapshot.members, view.members);
    assert_eq!(snapshot.pending_members, view.pending_members);
    assert_eq!(snapshot.pending, view.pending);
    assert_eq!(snapshot.state_version, view.state_version);
    assert_eq!(snapshot.role, MemberRole::Owner);
}

#[test]
fn invalid_requests_are_rejected_before_any_write() {
    let mut fixture = Fixture::new();
    let carol = member(&fixture.carol, "carol");

    // 1) 名称非法。
    for name in [
        String::new(),
        " leading".to_owned(),
        "trailing ".to_owned(),
        "with\u{0007}control".to_owned(),
        "x".repeat(MAX_GROUP_NAME_BYTES + 1),
    ] {
        let request = CreateGroupRequest {
            name,
            local_credential: "alice".to_owned(),
            policy_version: 1,
            members: Vec::new(),
        };
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &fixture.bob,
        };
        let error = create_group(&mut fixture.alice, &peer, &mut fixture.store, &request)
            .expect_err("invalid name");
        assert_eq!(error.code(), "INVALID_INPUT", "name validation code");
    }

    // 2) 重复凭据 / 重复 KeyPackage / 自己加自己 / 空 KeyPackage。
    let cases: Vec<(&str, CreateGroupRequest, &str)> = vec![
        (
            "duplicate credential",
            CreateGroupRequest {
                name: "dup".to_owned(),
                local_credential: "alice".to_owned(),
                policy_version: 1,
                members: vec![carol.clone(), carol.clone()],
            },
            "INVALID_INPUT",
        ),
        (
            "self add",
            CreateGroupRequest {
                name: "self".to_owned(),
                local_credential: "alice".to_owned(),
                policy_version: 1,
                members: vec![GroupMemberKeyPackage {
                    credential: "alice".to_owned(),
                    key_package: fixture.alice.key_package().expect("own key package"),
                }],
            },
            "INVALID_INPUT",
        ),
        (
            "empty key package",
            CreateGroupRequest {
                name: "empty".to_owned(),
                local_credential: "alice".to_owned(),
                policy_version: 1,
                members: vec![GroupMemberKeyPackage {
                    credential: "nobody".to_owned(),
                    key_package: Vec::new(),
                }],
            },
            "INVALID_CANDIDATE",
        ),
        (
            "unsupported policy",
            CreateGroupRequest {
                name: "policy".to_owned(),
                local_credential: "alice".to_owned(),
                policy_version: 9,
                members: Vec::new(),
            },
            "INVALID_POLICY_TRANSITION",
        ),
    ];
    for (label, request, expected) in cases {
        let peer = GroupBootstrapPeer {
            credential: "bob".to_owned(),
            device: &fixture.bob,
        };
        let error = create_group(&mut fixture.alice, &peer, &mut fixture.store, &request)
            .expect_err(label);
        assert_eq!(error.code(), expected, "case: {label}");
    }

    // 3) 成员超限（引导 1 + 追加 MAX-1 已等于上限，再多一个就拒绝）。
    let extras: Vec<GroupMemberKeyPackage> = (0..MAX_GROUP_MEMBERS - 1)
        .map(|index| GroupMemberKeyPackage {
            credential: format!("member-{index}"),
            key_package: fixture.carol.key_package().expect("key package"),
        })
        .collect();
    // 同一个 KeyPackage 重复出现会先命中“重复 KeyPackage”，所以这里用不同设备的包：
    let mut extras = extras;
    for (index, extra) in extras.iter_mut().enumerate() {
        let device = MlsDevice::new(&format!("extra-{index}")).expect("extra device");
        extra.key_package = device.key_package().expect("key package");
    }
    let over_limit = CreateGroupRequest {
        name: "too many".to_owned(),
        local_credential: "alice".to_owned(),
        policy_version: 1,
        members: extras,
    };
    let peer = GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &fixture.bob,
    };
    let error = create_group(&mut fixture.alice, &peer, &mut fixture.store, &over_limit)
        .expect_err("member limit");
    assert_eq!(error.code(), "GROUP_LIMIT_EXCEEDED");

    // 4) 所有失败都没有碰 MLS/库：设备仍然没有群，随后一次合法建群成功。
    assert!(fixture.alice.group_key().is_err(), "no group was bootstrapped");
    let view = fixture.create("after failures");
    assert_eq!(view.epoch, 1);
    assert_eq!(view.members.len(), 2);
}

#[test]
fn mls_level_rejection_leaves_no_pending_and_no_snapshot() {
    let mut fixture = Fixture::new();
    // 自己的 KeyPackage 混在追加成员里：预检无法逐字节识别（每次生成的 KeyPackage 都不同），
    // 由 CRYPTO-005 的 MLS 级校验拒绝（不能把自己加进群）。
    let own = fixture.alice.key_package().expect("own key package");
    let request = CreateGroupRequest {
        name: "self by mls".to_owned(),
        local_credential: "alice".to_owned(),
        policy_version: 1,
        members: vec![GroupMemberKeyPackage {
            credential: "alice-shadow".to_owned(),
            key_package: own,
        }],
    };
    let peer = GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &fixture.bob,
    };
    let error = create_group(&mut fixture.alice, &peer, &mut fixture.store, &request)
        .expect_err("adding the local device is rejected by MLS");
    assert_eq!(error.code(), "INVALID_STATE_TRANSITION");

    // 引导已经发生（真实群成立），但**没有 pending**、**没有快照记录**。
    assert!(fixture.alice.pending_control().expect("pending query").is_none());
    let group_key = fixture.alice.group_key().expect("group exists");
    let group_id = protocol_group_id_for(&group_key).expect("group id");
    let error = group_snapshot(&fixture.store, &group_id).expect_err("no snapshot record");
    assert_eq!(error.code(), "INVALID_TARGET");
}

#[test]
fn unknown_and_malformed_group_ids_are_rejected() {
    let fixture = Fixture::new();
    let unknown = GroupId::from_bytes([9_u8; 32]).encode();
    let error = group_snapshot(&fixture.store, &unknown).expect_err("unknown group");
    assert_eq!(error.code(), "INVALID_TARGET");

    for malformed in ["", "grp:1:zz", "grp:2:00", "not-a-group"] {
        let error = group_snapshot(&fixture.store, malformed).expect_err("malformed id");
        assert_eq!(error.code(), "INVALID_ID", "input: {malformed}");
    }
}

#[test]
fn corrupted_mls_state_is_reported_as_corrupted() {
    let mut fixture = Fixture::new();
    let view = fixture.create("损坏检测");
    // 用同一个 storage group_id（MLS 群键）覆盖成不可恢复的状态字节。
    fixture
        .store
        .save_mls_group_state(MlsGroupStateInput {
            group_id: view.mls_group_key.clone(),
            epoch: view.epoch,
            state: b"not-an-mls-group-header".to_vec(),
            records: Vec::new(),
        })
        .expect("overwrite mls state");
    let error = group_snapshot(&fixture.store, &view.group_id).expect_err("corrupted mls state");
    assert_eq!(error.code(), "STATE_CORRUPTED");
}
