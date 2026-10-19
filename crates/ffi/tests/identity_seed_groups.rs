//! GROUPS-CTRLREV FFI 用例：建群/入群自动播种 + 恢复导入后对所有群播种（真库 + 真 OpenMLS）。
//!
//! 覆盖：建群前账户已有撤销设备 → 视图 seeded_revoked_devices=1 且快照含该设备；
//! IdentityFfi.seed_revoked_devices_for_all_groups 更新所有群且重复调用返回 0；
//! 恢复导入成功后对库里已有群播种一次。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::groups_ffi::GroupFfi;
use chat_ffi::identity_ffi::IdentityFfi;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const NODE: &str = "node:1:0000000000000000000000000000000000000000000000000000000000000000";

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
            "chat-ffi-seed-groups-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }

    fn store(&self) -> chat_storage::formal::Store {
        open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store")
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

fn open_identity(dir: &FixtureDir) -> Arc<IdentityFfi> {
    IdentityFfi::open_persisted(dir.database(), master_key_hex(), NODE.to_owned())
        .expect("open persisted identity")
}

fn revoke(store: &mut chat_storage::formal::Store, account: &str, device: &str) {
    store
        .revoke_device(account, device, 1_700_000_000_000)
        .expect("revoke device");
}

#[test]
fn creating_a_group_seeds_the_revocation_set_and_the_list_shows_it() {
    let dir = FixtureDir::new();
    // 账户 alice 在建群前已有撤销设备（凭据即账户，因此用群里的本地凭据写撤销）。
    {
        let mut store = dir.store();
        revoke(&mut store, "alice", "dev:1:seed-aaa");
    }
    let groups = GroupFfi::open(dir.database(), master_key_hex(), None).expect("open group ffi");
    let view = groups
        .group_create("群一".to_owned(), "alice".to_owned(), "bob".to_owned(), Vec::new())
        .expect("create group");
    assert_eq!(
        view.seeded_revoked_devices, 1,
        "group creation seeds the account revocation set"
    );
    let listed = groups.group_list().expect("group list");
    assert_eq!(listed.skipped, 0);
    assert_eq!(listed.groups[0].revoked_devices_count, 1);
    groups.shutdown();
}

#[test]
fn seeding_all_groups_updates_every_group_and_is_idempotent() {
    let dir = FixtureDir::new();
    {
        let groups = GroupFfi::open(dir.database(), master_key_hex(), None).expect("open");
        // 两个群都含 alice；撤销在**建群之后**写入，因此需要显式播种。
        groups
            .group_create("群一".to_owned(), "alice".to_owned(), "bob".to_owned(), Vec::new())
            .expect("create one");
        groups
            .group_create("群二".to_owned(), "alice".to_owned(), "bob".to_owned(), Vec::new())
            .expect("create two");
        groups.shutdown();
    }
    {
        let mut store = dir.store();
        revoke(&mut store, "alice", "dev:1:seed-bbb");
        let listed = chat_core::groups::group_list(&store).expect("list");
        assert_eq!(listed.groups.len(), 2);
        for summary in &listed.groups {
            assert_eq!(summary.revoked_devices_count, 0, "nothing seeded yet");
        }
    }
    let identity = open_identity(&dir);
    let seeded = identity
        .seed_revoked_devices_for_all_groups()
        .expect("seed all groups");
    assert_eq!(seeded, 2, "both groups receive the device");
    // 幂等：再调一次没有新增。
    assert_eq!(
        identity
            .seed_revoked_devices_for_all_groups()
            .expect("seed again"),
        0
    );
    identity.shutdown();
    // 落盘可见：列表计数都是 1。
    let store = dir.store();
    let listed = chat_core::groups::group_list(&store).expect("list after seeding");
    assert_eq!(listed.groups.len(), 2);
    for summary in &listed.groups {
        assert_eq!(summary.revoked_devices_count, 1);
    }
}

#[test]
fn a_recovery_import_seeds_the_groups_already_in_the_database() {
    // 源库：一份可导入的身份恢复文件。
    let source = FixtureDir::new();
    let (account_id, bundle) = {
        let identity = open_identity(&source);
        let account_id = identity.account().account_id;
        let bundle = identity
            .export_recovery_bundle("correct horse battery staple".to_owned())
            .expect("export bundle");
        identity.shutdown();
        (account_id, bundle)
    };

    // 目标库：已经有一个群（本地凭据就是源账户），且账户撤销集里有设备。
    let target = FixtureDir::new();
    {
        let groups = GroupFfi::open(target.database(), master_key_hex(), None).expect("open");
        groups
            .group_create(
                "导入前的群".to_owned(),
                account_id.clone(),
                "bob".to_owned(),
                Vec::new(),
            )
            .expect("create group before import");
        groups.shutdown();
    }
    {
        let mut store = target.store();
        revoke(&mut store, &account_id, "dev:1:import-aaa");
    }

    let import = IdentityFfi::open_recovery_target(target.database(), master_key_hex(), NODE.to_owned())
        .expect("open recovery target");
    import
        .import_recovery_bundle(
            "correct horse battery staple".to_owned(),
            bundle,
        )
        .expect("import the bundle");
    import.shutdown();

    // 导入后播种已经发生：库里那个群的撤销计数是 1（读完立刻释放写句柄）。
    {
        let store = target.store();
        let listed = chat_core::groups::group_list(&store).expect("list after import");
        assert_eq!(listed.groups.len(), 1);
        assert_eq!(listed.groups[0].revoked_devices_count, 1);
    }

    // 用恢复的身份再播一次：没有新增（幂等）。
    let identity = open_identity(&target);
    assert_eq!(
        identity
            .seed_revoked_devices_for_all_groups()
            .expect("seed after import"),
        0
    );
}
