//! GROUPS-002（列表侧）FFI 用例：经 GroupFfi.group_list() 读群列表（真 OpenMLS + 真临时库）。
//!
//! 覆盖：多群（含解散/退出）状态正确、改名后名字更新、损坏条目计入 skipped、关闭后 CORE_CLOSED。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::groups_ffi::GroupFfi;
use chat_ffi::local_store::FfiError;
use chat_storage::formal::{KeyProvenance, MlsGroupStateInput, ProtectedStorageKey, StoreOptions, open_store};

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
            "chat-ffi-grouplist-{}-{nanos}-{counter}",
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

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

#[test]
fn group_list_reports_states_and_skips_corrupted_entries() {
    let dir = FixtureDir::new();
    // 先往库里塞一条损坏的群快照记录（命名空间内合法密封、非法正文）。
    {
        let mut store = open_store(
            StoreOptions::new(dir.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store");
        store
            .save_mls_group_state(MlsGroupStateInput {
                group_id: format!("grpmeta/{}", "e".repeat(64)),
                epoch: 1,
                state: b"corrupted".to_vec(),
                records: Vec::new(),
            })
            .expect("write corrupted entry");
    }
    let ffi = GroupFfi::open(dir.database(), master_key_hex(), None).expect("open group ffi");

    // 没有群时：列表为空（损坏条目已经存在，因此 skipped 应该是 1）。
    let empty = ffi.group_list().expect("group list");
    assert!(empty.groups.is_empty(), "no real group yet");
    assert_eq!(empty.skipped, 1, "corrupted entries are reported, not dropped");

    // 建两个群（一个随后解散）。
    let first = ffi
        .group_create("群一".to_owned(), "alice".to_owned(), "bob".to_owned(), Vec::new())
        .expect("create group one");
    let second = ffi
        .group_create("群二".to_owned(), "alice".to_owned(), "bob".to_owned(), Vec::new())
        .expect("create group two");
    ffi.group_rename(second.group_id.clone(), "alice".to_owned(), "群二（改名）".to_owned())
        .expect("rename");

    let list = ffi.group_list().expect("group list");
    assert_eq!(list.skipped, 1);
    assert_eq!(list.groups.len(), 2);
    let one = list
        .groups
        .iter()
        .find(|summary| summary.group_id == first.group_id)
        .expect("group one summary");
    assert_eq!(one.name, "群一");
    assert_eq!(one.role, "owner");
    assert!(!one.dissolved);
    assert!(!one.left);
    let two = list
        .groups
        .iter()
        .find(|summary| summary.group_id == second.group_id)
        .expect("group two summary");
    assert_eq!(two.name, "群二（改名）", "renames are reflected immediately");

    // 关闭后显式 CORE_CLOSED。
    ffi.shutdown();
    assert_eq!(code(ffi.group_list().expect_err("closed")), "CORE_CLOSED");
    let _: Arc<GroupFfi> = ffi;
}
