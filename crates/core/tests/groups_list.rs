//! GROUPS-002（列表侧）：群列表查询（真 OpenMLS + 真临时库）。
//!
//! 覆盖：3 个群（含解散、已退出）→ 摘要状态正确；改名后列表名字更新；损坏条目被跳过且计数可见；
//! storage 的列表上限/前缀/倒序语义。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::groups::{
    CreateGroupRequest, GroupBootstrapPeer, GroupSummaryView, GroupView, accept_pending_control,
    create_group, dissolve_group, group_list, group_snapshot, leave_group, rename_group,
    snapshot_key_for, transfer_ownership,
};
use chat_crypto::engine::MlsDevice;
use chat_domain::MemberRole;
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
            "chat-core-grouplist-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.0.join("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open formal store")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn create(store: &mut Store, name: &str, local: &str, peer_credential: &str) -> GroupView {
    let mut device = MlsDevice::new(local).expect("local device");
    let peer = MlsDevice::new(peer_credential).expect("peer device");
    let request = CreateGroupRequest::new(name, local);
    let bootstrap = GroupBootstrapPeer {
        credential: peer_credential.to_owned(),
        device: &peer,
    };
    create_group(&mut device, &bootstrap, store, &request).expect("create group")
}

fn by_id(list: &chat_core::groups::GroupListView, group_id: &str) -> GroupSummaryView {
    list.groups
        .iter()
        .find(|summary| summary.group_id == group_id)
        .cloned()
        .expect("summary present")
}

#[test]
fn list_reports_each_group_state_and_reflects_renames() {
    let dir = FixtureDir::new();
    let mut store = dir.open();
    let normal = create(&mut store, "正常群", "alice", "bob");
    let dissolved = create(&mut store, "待解散群", "alice", "bob");
    let transferring = create(&mut store, "待退出群", "alice", "bob");

    dissolve_group(&mut store, &dissolved.group_id, "alice").expect("dissolve");

    // 退出：先转让群主给 bob 并接受，本机（alice）成为成员后退出。
    let mut device = MlsDevice::import_from_store(&store, &transferring.mls_group_key).expect("dev");
    let pending = transfer_ownership(
        &mut device,
        &mut store,
        &transferring.group_id,
        "alice",
        "bob",
    )
    .expect("transfer");
    assert!(pending.pending.is_some());
    accept_pending_control(&mut device, &mut store, &transferring.group_id).expect("accept");
    let left_view =
        leave_group(&mut device, &mut store, &transferring.group_id, "alice").expect("leave");
    assert!(left_view.left_at_ms.is_some());

    // 列表：三条都在，状态正确。
    let list = group_list(&store).expect("group list");
    assert_eq!(list.skipped, 0);
    assert_eq!(list.groups.len(), 3);

    let normal_summary = by_id(&list, &normal.group_id);
    assert_eq!(normal_summary.name, "正常群");
    assert_eq!(normal_summary.role, MemberRole::Owner);
    assert!(!normal_summary.dissolved);
    assert!(!normal_summary.left);
    assert!(!normal_summary.pending);

    let dissolved_summary = by_id(&list, &dissolved.group_id);
    assert!(dissolved_summary.dissolved);
    assert!(!dissolved_summary.left);

    let left_summary = by_id(&list, &transferring.group_id);
    assert!(left_summary.left);
    assert!(!left_summary.dissolved);
    assert_eq!(left_summary.role, MemberRole::Member);

    // 改名后名字立即更新（列表读最新快照），且与快照的版本/epoch 一致。
    rename_group(&mut store, &normal.group_id, "alice", "改名后的群").expect("rename");
    let list = group_list(&store).expect("group list");
    let renamed = by_id(&list, &normal.group_id);
    assert_eq!(renamed.name, "改名后的群");
    let snapshot = group_snapshot(&store, &normal.group_id).expect("snapshot");
    assert_eq!(renamed.policy_version, snapshot.policy_version);
    assert_eq!(renamed.epoch, snapshot.epoch);
}

#[test]
fn corrupted_entries_are_skipped_and_reported() {
    let dir = FixtureDir::new();
    let mut store = dir.open();
    let normal = create(&mut store, "正常群", "alice", "bob");

    // 往同一个命名空间写一条**额外**的、不是群快照的记录（合法密封、非法正文）。
    // 用独立的键，避免覆盖真实快照。
    let corrupt_key = format!("grpmeta/{}", "f".repeat(64));
    assert_ne!(corrupt_key, snapshot_key_for(&normal.group_id));
    store
        .save_mls_group_state(MlsGroupStateInput {
            group_id: corrupt_key,
            epoch: 1,
            state: b"not-a-group-snapshot".to_vec(),
            records: Vec::new(),
        })
        .expect("write a corrupted entry");

    let list = group_list(&store).expect("group list");
    assert_eq!(list.groups.len(), 1, "只有完好的那条被列出");
    assert_eq!(list.skipped, 1, "损坏条目计入 skipped，不静默丢弃");
    assert_eq!(list.groups[0].group_id, normal.group_id);
    // 新字段在损坏条目场景下也保持确定值（不因跳过错位）。
    assert_eq!(list.groups[0].muted_members, 0);
    assert!(!list.groups[0].pending_proposal);
}
#[test]
fn list_reports_muted_members() {
    let dir = FixtureDir::new();
    let mut store = dir.open();
    let group = create(&mut store, "禁言群", "alice", "bob");
    // 初始：没有禁言、没有待打包提案。
    let list = group_list(&store).expect("group list");
    let summary = by_id(&list, &group.group_id);
    assert_eq!(summary.muted_members, 0);
    assert!(!summary.pending_proposal);
    assert!(!summary.pending);

    // 禁言一名成员（bob）→ muted_members = 1；禁言不是"待打包提案"。
    // 注意：core 目前没有"邀请成员"入口（建群只带一个 bootstrap peer，且群主不能禁言自己），
    // 因此端到端只能构造 1 人被禁言；计数逻辑本身按**确认成员全量**统计。
    chat_core::groups::set_muted(&mut store, &group.group_id, "alice", "bob", true)
        .expect("mute bob");
    let list = group_list(&store).expect("group list");
    let summary = by_id(&list, &group.group_id);
    assert_eq!(summary.muted_members, 1, "the muted confirmed member is counted");
    assert!(!summary.pending_proposal, "muting is not a pending proposal");

    // 解除 → 归 0。
    chat_core::groups::set_muted(&mut store, &group.group_id, "alice", "bob", false)
        .expect("unmute bob");
    let list = group_list(&store).expect("group list");
    assert_eq!(by_id(&list, &group.group_id).muted_members, 0);
}


#[test]
fn storage_list_bounds_are_enforced() {
    let dir = FixtureDir::new();
    let mut store = dir.open();
    create(&mut store, "群一", "alice", "bob");
    create(&mut store, "群二", "alice", "bob");
    create(&mut store, "群三", "alice", "bob");

    // limit 越界显式拒绝（不夹取）。
    assert!(store.list_mls_group_state_keys("grpmeta/", 0).is_err());
    assert!(store.list_mls_group_state_keys("grpmeta/", 257).is_err());
    // 上限生效：limit = 2 只返回 2 条。
    let keys = store
        .list_mls_group_state_keys("grpmeta/", 2)
        .expect("bounded list");
    assert_eq!(keys.len(), 2);
    // 前缀过滤：其它前缀什么都取不到。
    let none = store
        .list_mls_group_state_keys("other/", 8)
        .expect("prefix filter");
    assert!(none.is_empty());
    // 全量列出后按更新时间倒序。
    let all = store
        .list_mls_group_state_keys("grpmeta/", 256)
        .expect("full list");
    assert_eq!(all.len(), 3);
    for window in all.windows(2) {
        assert!(
            window[0].updated_at_ms >= window[1].updated_at_ms,
            "list must be ordered by updated_at descending"
        );
    }
}
