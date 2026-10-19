//! IDENTITY-013/014：身份恢复文件导出/导入（真临时加密库）。
//!
//! 覆盖：导出 → 空库导入 → 公钥/ID/显示名一致且既有查询可读；错误口令/篡改/版本不支持/
//! 目标库非空四种拒绝且**零写入**；文件越界与口令越界拒绝；参数白名单。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_identity::keys::{DeviceKeys, KEY_SEED_BYTES};
use chat_identity::recovery::{
    ALLOWED_ARGON2_PARAMS, Argon2Params, MAX_RECOVERY_BUNDLE_BYTES, RecoveryError,
    export_recovery_bundle, export_recovery_bundle_with_params, import_recovery_bundle,
};
use chat_identity::service::LocalAccount;
use chat_storage::formal::{
    IdentityAccountInput, KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store,
};

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const ACCOUNT_SEED: [u8; KEY_SEED_BYTES] = [0x22; KEY_SEED_BYTES];
const DEVICE_SEED: [u8; KEY_SEED_BYTES] = [0x33; KEY_SEED_BYTES];
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
            "chat-identity-recovery-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
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

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 建一个已入库身份（固定种子），返回设备公钥 hex。
fn seed_identity(store: &mut Store) -> String {
    let account = LocalAccount::from_seed("恢复测试", 1_700_000_000_000, &ACCOUNT_SEED).expect("account");
    let device = DeviceKeys::from_seed(&DEVICE_SEED);
    let account_id = account.account_id();
    let device_id = device.device_id(account_id);
    store
        .save_identity_account(IdentityAccountInput {
            account_id: account_id.encode(),
            device_id: device_id.encode(),
            account_public_key: *account.trust_anchor().account_public_key(),
            device_public_key: device.public_key(),
            account_seed: ACCOUNT_SEED.to_vec(),
            device_seed: DEVICE_SEED.to_vec(),
            display_name: "恢复测试".to_owned(),
            fingerprint: *account.fingerprint().as_bytes(),
            created_at_ms: 1_700_000_000_000,
        })
        .expect("save identity");
    hex_of(&device.public_key())
}

#[test]
fn export_then_import_restores_the_same_identity() {
    let source_dir = FixtureDir::new();
    let mut source = source_dir.open();
    let device_public_key = seed_identity(&mut source);
    let source_account = source.load_identity_account().expect("account").expect("present");

    let bundle = export_recovery_bundle(&source, PASSPHRASE).expect("export");
    assert!(bundle.len() > 100, "bundle carries header + ciphertext");
    assert!(bundle.len() <= MAX_RECOVERY_BUNDLE_BYTES);
    // 恢复文件里不该出现明文种子或主密钥字节。
    let plain_account_seed = ACCOUNT_SEED;
    assert!(
        !bundle.windows(32).any(|window| window == plain_account_seed),
        "account seed must never appear in plaintext"
    );
    assert!(
        !bundle.windows(32).any(|window| window == MASTER_KEY),
        "storage master key must never be part of the bundle"
    );

    // 导入到空库：身份一致，且既有查询能读到。
    let target_dir = FixtureDir::new();
    let mut target = target_dir.open();
    assert!(target.load_identity_account().expect("probe").is_none());
    let view = import_recovery_bundle(&mut target, &bundle, PASSPHRASE).expect("import");
    assert_eq!(view.account_id, source_account.account_id);
    assert_eq!(view.device_id, source_account.device_id);
    assert_eq!(view.device_public_key, device_public_key);
    assert_eq!(view.display_name, "恢复测试");
    assert_eq!(view.created_at_ms, 1_700_000_000_000);
    let restored = target.load_identity_account().expect("account").expect("present");
    assert_eq!(restored.account_id, source_account.account_id);
    assert_eq!(restored.device_id, source_account.device_id);
    // 种子也真的能再次签名：从库里读回种子重建设备密钥，公钥必须一致。
    let seeds = target.load_identity_seeds().expect("seeds").expect("present");
    let rebuilt = DeviceKeys::from_seed(
        seeds.device_seed.as_slice().try_into().expect("32 bytes"),
    );
    assert_eq!(hex_of(&rebuilt.public_key()), device_public_key);
}

#[test]
fn four_rejections_leave_the_target_untouched() {
    let source_dir = FixtureDir::new();
    let mut source = source_dir.open();
    seed_identity(&mut source);
    let bundle = export_recovery_bundle(&source, PASSPHRASE).expect("export");

    // 1) 错误口令 → RECOVERY_BAD_PASSPHRASE，零写入。
    let target_dir = FixtureDir::new();
    let mut target = target_dir.open();
    let error = import_recovery_bundle(&mut target, &bundle, "wrong passphrase!!").expect_err("bad passphrase");
    assert_eq!(error.code(), "RECOVERY_BAD_PASSPHRASE");
    assert!(target.load_identity_account().expect("probe").is_none());
    assert!(target.load_identity_seeds().expect("probe").is_none());

    // 2) 篡改正文 → RECOVERY_CORRUPTED。
    let mut tampered = bundle.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    let error = import_recovery_bundle(&mut target, &tampered, PASSPHRASE).expect_err("tampered");
    assert_eq!(error.code(), "RECOVERY_CORRUPTED");
    assert!(target.load_identity_account().expect("probe").is_none());

    // 3) 版本不支持 → RECOVERY_UNSUPPORTED。
    let mut newer = bundle.clone();
    newer[8] = 9;
    let error = import_recovery_bundle(&mut target, &newer, PASSPHRASE).expect_err("unsupported");
    assert_eq!(error.code(), "RECOVERY_UNSUPPORTED");

    // 4) 目标库非空 → RECOVERY_TARGET_NOT_EMPTY（绝不覆盖）。
    let busy_dir = FixtureDir::new();
    let mut busy = busy_dir.open();
    let existing = seed_identity(&mut busy);
    let error = import_recovery_bundle(&mut busy, &bundle, PASSPHRASE).expect_err("target not empty");
    assert_eq!(error.code(), "RECOVERY_TARGET_NOT_EMPTY");
    let after = busy.load_identity_account().expect("account").expect("present");
    assert_eq!(hex_of(&DeviceKeys::from_seed(&DEVICE_SEED).public_key()), existing);
    assert_eq!(after.device_id, DeviceKeys::from_seed(&DEVICE_SEED).device_id(after_account(&after.account_id)).encode());
}

fn after_account(text: &str) -> chat_protocol::AccountId {
    chat_protocol::AccountId::parse(text).expect("account id")
}

#[test]
fn oversized_inputs_and_unknown_kdf_parameters_are_rejected() {
    let source_dir = FixtureDir::new();
    let mut source = source_dir.open();
    seed_identity(&mut source);
    let bundle = export_recovery_bundle(&source, PASSPHRASE).expect("export");

    // 文件超过 1 MiB → INVALID_INPUT（在解密之前拒绝）。
    let oversized = vec![0_u8; MAX_RECOVERY_BUNDLE_BYTES + 1];
    let target_dir = FixtureDir::new();
    let mut target = target_dir.open();
    let error = import_recovery_bundle(&mut target, &oversized, PASSPHRASE).expect_err("too large");
    assert_eq!(error.code(), "INVALID_INPUT");
    assert!(target.load_identity_account().expect("probe").is_none());

    // 口令越界。
    let error = export_recovery_bundle(&source, "short").expect_err("short passphrase");
    assert_eq!(error.code(), "INVALID_INPUT");
    let long = "a".repeat(257);
    let error = export_recovery_bundle(&source, &long).expect_err("long passphrase");
    assert_eq!(error.code(), "INVALID_INPUT");

    // argon2 参数不在白名单：导出拒绝、导入也拒绝。
    let rogue = Argon2Params { m_cost_kib: 8, t_cost: 1, p_cost: 1 };
    assert!(!rogue.is_allowed());
    let error = export_recovery_bundle_with_params(&source, PASSPHRASE, rogue).expect_err("rogue params");
    assert_eq!(error.code(), "RECOVERY_UNSUPPORTED");
    // 白名单参数都能导出，并且导出的文件能被导入。
    for params in ALLOWED_ARGON2_PARAMS {
        let bundle = export_recovery_bundle_with_params(&source, PASSPHRASE, params).expect("export");
        let dir = FixtureDir::new();
        let mut store = dir.open();
        import_recovery_bundle(&mut store, &bundle, PASSPHRASE).expect("import with allowed params");
    }
    // 头部声明的参数被改成白名单外的值 → 导入拒绝。
    let mut rogue_header = bundle.clone();
    rogue_header[9..13].copy_from_slice(&8_u32.to_be_bytes());
    let error = import_recovery_bundle(&mut target, &rogue_header, PASSPHRASE).expect_err("rogue header");
    assert_eq!(error.code(), "RECOVERY_UNSUPPORTED");
}

#[test]
fn export_without_an_identity_is_rejected() {
    let dir = FixtureDir::new();
    let store = dir.open();
    let error = export_recovery_bundle(&store, PASSPHRASE).expect_err("no identity");
    assert_eq!(error.code(), "INVALID_INPUT");
    // 错误本身不带任何密钥材料。
    let detail = error.detail();
    assert!(!detail.is_empty());
    let _ = RecoveryError::Corrupted;
}
