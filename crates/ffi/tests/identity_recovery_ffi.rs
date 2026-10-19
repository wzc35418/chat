//! 身份恢复文件 FFI 用例（真临时加密库）。
//!
//! 主路径：open_persisted → 导出 → shutdown → open_recovery_target(空库) → import →
//! open_persisted 重开 → 读到同一账户/设备公钥。
//! 拒绝路径：错口令 / 篡改 / 目标库非空 / 未关闭 / 非持久化对象 / shutdown 后 CORE_CLOSED。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chat_ffi::identity_ffi::IdentityFfi;
use chat_ffi::local_store::FfiError;

const MASTER_KEY_HEX: &str =
    "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
const NODE: &str = "node:1:0000000000000000000000000000000000000000000000000000000000000000";
const PASSPHRASE: &str = "correct horse battery";

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
            "chat-ffi-identity-recovery-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }

    fn open(&self) -> Arc<IdentityFfi> {
        IdentityFfi::open_persisted(self.database(), MASTER_KEY_HEX.to_owned(), NODE.to_owned())
            .expect("open persisted identity")
    }

    fn bind_recovery_target(&self) -> Arc<IdentityFfi> {
        IdentityFfi::open_recovery_target(
            self.database(),
            MASTER_KEY_HEX.to_owned(),
            NODE.to_owned(),
        )
        .expect("bind empty recovery target")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

#[test]
fn export_then_import_restores_the_same_identity() {
    let source_dir = FixtureDir::new();
    let source = source_dir.open();
    let original = source.account();
    let bundle = source.export_recovery_bundle(PASSPHRASE.to_owned()).expect("export");
    assert!(bundle.len() > 100, "bundle carries header + ciphertext");
    // 导出会短暂释放/重开写锁，身份仍然可用。
    assert_eq!(source.account().device_id, original.device_id);
    source.shutdown();

    // 新机器：先在空库上绑定恢复目标，再导入。
    let target_dir = FixtureDir::new();
    let target = target_dir.bind_recovery_target();
    let view = target
        .import_recovery_bundle(PASSPHRASE.to_owned(), bundle.clone())
        .expect("import");
    assert_eq!(view.account_id, original.account_id);
    assert_eq!(view.device_id, original.device_id);
    assert_eq!(view.display_name, original.display_name);
    assert!(!view.devices_digest.is_empty());

    // 重开同一库：读到的就是恢复出来的身份。
    let reopened = target_dir.open();
    let restored = reopened.account();
    assert_eq!(restored.account_id, original.account_id);
    assert_eq!(restored.device_id, original.device_id);
    assert_eq!(restored.display_name, original.display_name);
    assert_eq!(restored.fingerprint, original.fingerprint);
}

#[test]
fn rejections_leave_the_target_usable_and_unchanged() {
    let source_dir = FixtureDir::new();
    let source = source_dir.open();
    let bundle = source.export_recovery_bundle(PASSPHRASE.to_owned()).expect("export");
    source.shutdown();

    // 1) 错口令 → RECOVERY_BAD_PASSPHRASE；随后用正确口令仍能导入（证明没有写入）。
    let dir = FixtureDir::new();
    let target = dir.bind_recovery_target();
    let error = target
        .import_recovery_bundle("wrong passphrase!!".to_owned(), bundle.clone())
        .expect_err("bad passphrase");
    assert_eq!(code(error), "RECOVERY_BAD_PASSPHRASE");
    target
        .import_recovery_bundle(PASSPHRASE.to_owned(), bundle.clone())
        .expect("untouched target still accepts the real bundle");

    // 2) 篡改正文 → RECOVERY_CORRUPTED；同样不写坏目标库。
    let dir2 = FixtureDir::new();
    let target2 = dir2.bind_recovery_target();
    let mut tampered = bundle.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    let error = target2
        .import_recovery_bundle(PASSPHRASE.to_owned(), tampered)
        .expect_err("tampered");
    assert_eq!(code(error), "RECOVERY_CORRUPTED");
    target2
        .import_recovery_bundle(PASSPHRASE.to_owned(), bundle.clone())
        .expect("target still accepts the real bundle");

    // 3) 超限文件 → INVALID_INPUT（解密前拒绝）。
    let dir3 = FixtureDir::new();
    let target3 = dir3.bind_recovery_target();
    let oversized = vec![0_u8; chat_identity::MAX_RECOVERY_BUNDLE_BYTES + 1];
    let error = target3
        .import_recovery_bundle(PASSPHRASE.to_owned(), oversized)
        .expect_err("too large");
    assert_eq!(code(error), "INVALID_INPUT");

    // 4) 目标库非空：已打开（未关闭）→ INVALID_STATE_TRANSITION。
    let busy_dir = FixtureDir::new();
    let busy = busy_dir.open();
    let busy_device = busy.account().device_id;
    let error = busy
        .import_recovery_bundle(PASSPHRASE.to_owned(), bundle.clone())
        .expect_err("must close first");
    assert_eq!(code(error), "INVALID_STATE_TRANSITION");
    // 关闭后导入 → RECOVERY_TARGET_NOT_EMPTY（绝不覆盖）。
    busy.shutdown();
    let error = busy
        .import_recovery_bundle(PASSPHRASE.to_owned(), bundle.clone())
        .expect_err("target not empty");
    assert_eq!(code(error), "RECOVERY_TARGET_NOT_EMPTY");
    // 原身份不变。
    drop(busy);
    let reopened = busy_dir.open();
    assert_eq!(reopened.account().device_id, busy_device);

    // 5) 空库上 bind 之后又建了身份 → bind 直接拒绝。
    let dir4 = FixtureDir::new();
    let existing = dir4.open();
    existing.shutdown();
    let error = match IdentityFfi::open_recovery_target(
        dir4.database(),
        MASTER_KEY_HEX.to_owned(),
        NODE.to_owned(),
    ) {
        Ok(_) => panic!("a store with an identity must not be bound as a recovery target"),
        Err(error) => error,
    };
    assert_eq!(code(error), "RECOVERY_TARGET_NOT_EMPTY");
}

#[test]
fn export_requires_a_persisted_open_identity() {
    // 非持久化对象（create）没有库：export 显式 INVALID_INPUT。
    let in_memory = IdentityFfi::create("内存身份".to_owned(), NODE.to_owned()).expect("create");
    let error = in_memory
        .export_recovery_bundle(PASSPHRASE.to_owned())
        .expect_err("not persisted");
    assert_eq!(code(error), "INVALID_INPUT");
    // 恢复目标对象是关闭态：export 一律 CORE_CLOSED。
    let dir = FixtureDir::new();
    let target = dir.bind_recovery_target();
    let error = target
        .export_recovery_bundle(PASSPHRASE.to_owned())
        .expect_err("closed");
    assert_eq!(code(error), "CORE_CLOSED");
    // 口令越界也要显式失败。
    let source_dir = FixtureDir::new();
    let source = source_dir.open();
    let error = source.export_recovery_bundle("short".to_owned()).expect_err("short passphrase");
    assert_eq!(code(error), "INVALID_INPUT");
    source.shutdown();
    // shutdown 之后 export 一律 CORE_CLOSED（account() 仍返回关闭前快照）。
    let error = source
        .export_recovery_bundle(PASSPHRASE.to_owned())
        .expect_err("closed after shutdown");
    assert_eq!(code(error), "CORE_CLOSED");
    assert!(!source.account().account_id.is_empty());
}
