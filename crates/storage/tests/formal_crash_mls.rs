//! MLS 状态写入的真实进程终止（abort）崩溃断点测试。
//!
//! 子进程在群状态/记录替换/提交前三个断点上调用 std::process::abort()，父进程重开
//! 同一个文件库并断言：要么是上一 epoch 的完整状态，要么是新 epoch 的完整状态，
//! 绝不出现“群状态已换、记录未换”或“记录半套”的半状态。

use chat_storage::formal::{
    KeyProvenance, MlsGroupStateInput, MlsRecordInput, ProtectedStorageKey, StoreOptions,
    open_store,
};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const GROUP: &str = "group-crash";

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("chat-crash-mls-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).expect("create fixture directory");
        let path = dir.join("chat.db");
        let mut key_bytes = [0u8; 32];
        key_bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        key_bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self {
            dir,
            path,
            key_bytes,
        }
    }

    fn key(&self) -> ProtectedStorageKey {
        ProtectedStorageKey::from_bytes(self.key_bytes)
    }

    fn key_hex(&self) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.key_bytes {
            hex.push_str(&format!("{byte:02x}"));
        }
        hex
    }

    fn open(&self) -> chat_storage::formal::Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            self.key(),
        )
        .expect("open formal store")
    }

    fn crash(&self, arguments: &[&str]) {
        let output = Command::new(env!("CARGO_BIN_EXE_storage-crash-worker"))
            .arg(arguments[0])
            .arg(&self.path)
            .arg(self.key_hex())
            .args(&arguments[1..])
            .output()
            .expect("run crash worker");
        assert!(
            !output.status.success(),
            "crash worker must not exit successfully"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("STORAGE_CRASH_CHECKPOINT="),
            "worker did not reach a checkpoint: {stderr}"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn committed_epoch_one(fixture: &Fixture) {
    let mut store = fixture.open();
    store
        .save_mls_group_state(MlsGroupStateInput {
            group_id: GROUP.to_owned(),
            epoch: 1,
            state: b"state-epoch-1".to_vec(),
            records: vec![MlsRecordInput {
                record_key: b"record-1".to_vec(),
                value: b"secret-epoch-1".to_vec(),
            }],
        })
        .expect("commit epoch 1");
}

#[test]
fn mls_export_abort_before_commit_leaves_the_previous_epoch_only() {
    for checkpoint in ["state", "records", "commit"] {
        let fixture = Fixture::new();
        committed_epoch_one(&fixture);
        fixture.crash(&[
            "mls",
            checkpoint,
            GROUP,
            "2",
            "state-epoch-2",
            "record-2",
            "secret-epoch-2",
        ]);

        let mut store = fixture.open();
        let state = store
            .load_mls_group_state(GROUP)
            .expect("load")
            .expect("epoch 1 must survive");
        assert_eq!(state.epoch, 1, "abort at {checkpoint} must not advance epoch");
        assert_eq!(state.state, b"state-epoch-1");
        let records = store.load_mls_records(GROUP).expect("records");
        assert_eq!(records.len(), 1, "no half record set at {checkpoint}");
        assert_eq!(records[0].record_key, b"record-1");
        assert_eq!(records[0].value, b"secret-epoch-1");
        assert!(
            store
                .load_mls_record(GROUP, b"record-2")
                .expect("no aborted record")
                .is_none()
        );

        // 断点之后同一写入可以正常完成（abort 只丢本次事务）。
        store
            .save_mls_group_state(MlsGroupStateInput {
                group_id: GROUP.to_owned(),
                epoch: 2,
                state: b"state-epoch-2".to_vec(),
                records: vec![MlsRecordInput {
                    record_key: b"record-2".to_vec(),
                    value: b"secret-epoch-2".to_vec(),
                }],
            })
            .expect("retry epoch 2");
        assert_eq!(
            store
                .load_mls_group_state(GROUP)
                .expect("load")
                .expect("present")
                .epoch,
            2
        );
        assert!(
            store
                .load_mls_record(GROUP, b"record-1")
                .expect("retired record")
                .is_none()
        );
    }
}
