
//! 身份设备凭据与撤销集合的正式存储测试（迁移 0006_device_credentials）。
//!
//! 覆盖：保存→重开读回、幂等、密封列篡改/挪行/改时间列 STATE_CORRUPTED、
//! 撤销终态与幂等（不可静默复活）、撤销版本单调、跨账户隔离、上限与非法输入。

use chat_storage::formal::{
    DeviceCredentialInput, KeyProvenance, MAX_DEVICE_CREDENTIALS, MAX_DEVICE_REVOCATIONS,
    ProtectedStorageKey, Store, StoreOptions, open_store,
};
use rusqlite::params;
use std::fs;
use std::path::PathBuf;

const ACCOUNT: &str = "acct-1";
const DEVICE: &str = "dev-1";
const DEVICE_TWO: &str = "dev-2";
const OTHER_ACCOUNT: &str = "acct-2";
const ISSUED_AT: i64 = 1_700_000_000_000;
const EXPIRES_AT: i64 = ISSUED_AT + 60_000;

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key_bytes: [u8; 32],
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "chat-formal-devices-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
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

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            self.key(),
        )
        .expect("open formal store")
    }

    fn store_files(&self) -> Vec<Vec<u8>> {
        let mut files = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", self.path.display()));
            if let Ok(bytes) = fs::read(path) {
                files.push(bytes);
            }
        }
        files
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn credential(account: &str, device: &str, public_key: u8) -> DeviceCredentialInput {
    DeviceCredentialInput {
        account_id: account.to_owned(),
        device_id: device.to_owned(),
        device_public_key: [public_key; 32],
        endpoint: Some([0x5a; 32]),
        capability_mask: 0b0000_1110,
        issued_at_ms: ISSUED_AT,
        expires_at_ms: EXPIRES_AT,
        revocation_generation: 0,
        signature: [0x33; 64],
    }
}

#[test]
fn device_credential_roundtrip_survives_reopen() {
    let fixture = Fixture::new("roundtrip");
    let mut store = fixture.open();
    let saved = store
        .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
        .expect("save device credential");
    assert!(!saved.already_existed);
    assert_eq!(saved.account_id, ACCOUNT);
    assert_eq!(saved.device_id, DEVICE);
    assert_eq!(saved.device_public_key, [0x21; 32]);
    assert_eq!(saved.endpoint, Some([0x5a; 32]));
    assert_eq!(saved.capability_mask, 0b0000_1110);
    assert_eq!(saved.issued_at_ms, ISSUED_AT);
    assert_eq!(saved.expires_at_ms, EXPIRES_AT);
    assert_eq!(saved.signature, [0x33; 64]);
    assert!(!saved.revoked);

    // 完全相同再次保存：幂等，不重复写。
    let again = store
        .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
        .expect("idempotent save");
    assert!(again.already_existed);

    let listed = store.list_device_credentials(ACCOUNT).expect("list");
    assert_eq!(listed.len(), 1);
    drop(store);

    // 重开：记录仍在且字段一致。
    let store = fixture.open();
    let loaded = store
        .load_device_credential(ACCOUNT, DEVICE)
        .expect("load")
        .expect("present");
    let mut expected = saved.clone();
    // 读取路径的 already_existed 恒为 true。
    expected.already_existed = true;
    assert_eq!(loaded, expected);
    assert_eq!(
        store.list_device_credentials(ACCOUNT).expect("list"),
        vec![expected]
    );
    assert_eq!(store.device_revocation_generation(ACCOUNT).expect("epoch"), 0);
    assert!(store.load_device_credential(ACCOUNT, DEVICE_TWO).expect("load").is_none());
    assert!(store.list_device_credentials(OTHER_ACCOUNT).expect("list").is_empty());

    // 磁盘上不出现设备公钥明文（密封列）。
    let public_key = [0x21u8; 32];
    for bytes in fixture.store_files() {
        assert!(
            !contains(&bytes, &public_key),
            "device public key must not be stored in plaintext"
        );
        assert!(!contains(&bytes, ACCOUNT.as_bytes()));
        assert!(!contains(&bytes, DEVICE.as_bytes()));
    }
}

#[test]
fn duplicate_and_renewal_semantics() {
    let fixture = Fixture::new("renewal");
    let mut store = fixture.open();
    store
        .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
        .expect("save");

    // 换设备公钥（等于换设备身份）：拒绝，不做隐式替换。
    let mut swapped = credential(ACCOUNT, DEVICE, 0x22);
    swapped.capability_mask = 0b0000_1110;
    assert_eq!(
        store.save_device_credential(swapped).unwrap_err().code(),
        "INVALID_INPUT"
    );

    // 续期：同公钥、签发时间前移（更晚）→ 更新；更早回退 → 拒绝。
    let mut renewed = credential(ACCOUNT, DEVICE, 0x21);
    renewed.issued_at_ms = ISSUED_AT + 1_000;
    renewed.expires_at_ms = EXPIRES_AT + 1_000;
    let renewed_record = store.save_device_credential(renewed.clone()).expect("renew");
    assert!(renewed_record.already_existed);
    assert_eq!(renewed_record.issued_at_ms, ISSUED_AT + 1_000);
    let loaded = store
        .load_device_credential(ACCOUNT, DEVICE)
        .expect("load")
        .expect("present");
    assert_eq!(loaded.issued_at_ms, ISSUED_AT + 1_000);

    let mut backwards = credential(ACCOUNT, DEVICE, 0x21);
    backwards.expires_at_ms = EXPIRES_AT + 2_000;
    assert_eq!(
        store.save_device_credential(backwards).unwrap_err().code(),
        "INVALID_STATE_TRANSITION"
    );
}

#[test]
fn tampered_device_records_fail_closed() {
    // 每种篡改都用独立库，避免"改到哪一行"的不确定性。
    // 1) 翻转密封字节。
    {
        let fixture = Fixture::new("tamper-sealed");
        let mut store = fixture.open();
        store
            .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
            .expect("save");
        let sealed: Vec<u8> = store
            .connection()
            .query_row(
                "SELECT protected_credential FROM identity_device_credentials",
                [],
                |row| row.get(0),
            )
            .expect("sealed");
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        store
            .connection()
            .execute(
                "UPDATE identity_device_credentials SET protected_credential = ?1",
                params![tampered],
            )
            .expect("tamper");
        assert_eq!(
            store
                .load_device_credential(ACCOUNT, DEVICE)
                .unwrap_err()
                .code(),
            "STATE_CORRUPTED"
        );
        // 篡改后列表也必须显式失败，不能静默跳过。
        assert_eq!(
            store.list_device_credentials(ACCOUNT).unwrap_err().code(),
            "STATE_CORRUPTED"
        );
    }

    // 2) 把另一行的密封密文挪到所有行：AAD 绑定设备索引，必须失败。
    {
        let fixture = Fixture::new("tamper-relocate");
        let mut store = fixture.open();
        store
            .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
            .expect("save first");
        store
            .save_device_credential(credential(ACCOUNT, DEVICE_TWO, 0x22))
            .expect("save second");
        let sealed: Vec<u8> = store
            .connection()
            .query_row(
                "SELECT protected_credential FROM identity_device_credentials
                  ORDER BY device_index ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("sealed");
        store
            .connection()
            .execute(
                "UPDATE identity_device_credentials SET protected_credential = ?1",
                params![sealed],
            )
            .expect("relocate");
        // 两行共用同一份密文：只可能有一行（密文真正的主人）通过认证，
        // 另一行必须显式 STATE_CORRUPTED，绝不允许"用别的设备的密文通过校验"。
        let loads = [
            store.load_device_credential(ACCOUNT, DEVICE),
            store.load_device_credential(ACCOUNT, DEVICE_TWO),
        ];
        let corrupted = loads
            .iter()
            .filter(|result| matches!(result, Err(error) if error.code() == "STATE_CORRUPTED"))
            .count();
        assert_eq!(
            corrupted, 1,
            "relocated sealed record must fail authentication for the other device"
        );
    }

    // 3) 只改明文时间列（与密封记录不一致）。
    {
        let fixture = Fixture::new("tamper-timestamp");
        let mut store = fixture.open();
        store
            .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
            .expect("save");
        let sealed: Vec<u8> = store
            .connection()
            .query_row(
                "SELECT protected_credential FROM identity_device_credentials",
                [],
                |row| row.get(0),
            )
            .expect("sealed");
        store
            .connection()
            .execute(
                "UPDATE identity_device_credentials
                    SET protected_credential = ?1, issued_at = ?2",
                params![sealed, ISSUED_AT + 5],
            )
            .expect("timestamp tamper");
        assert_eq!(
            store
                .load_device_credential(ACCOUNT, DEVICE)
                .unwrap_err()
                .code(),
            "STATE_CORRUPTED"
        );
    }
}

#[test]
fn revocation_is_terminal_and_idempotent() {
    let fixture = Fixture::new("revoke");
    let mut store = fixture.open();
    store
        .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
        .expect("save");
    store
        .save_device_credential(credential(ACCOUNT, DEVICE_TWO, 0x22))
        .expect("save second");

    let first = store
        .revoke_device(ACCOUNT, DEVICE, ISSUED_AT + 10)
        .expect("revoke");
    assert!(!first.already_existed);
    assert_eq!(first.revocation_generation, 1);
    assert_eq!(first.device_id, DEVICE);

    // 重复撤销：幂等，版本与时间都不变。
    let repeat = store
        .revoke_device(ACCOUNT, DEVICE, ISSUED_AT + 99)
        .expect("idempotent revoke");
    assert!(repeat.already_existed);
    assert_eq!(repeat.revocation_generation, 1);
    assert_eq!(repeat.revoked_at_ms, ISSUED_AT + 10);
    assert_eq!(store.device_revocation_generation(ACCOUNT).expect("epoch"), 1);

    // 已撤销设备：读回带 revoked 标志，且不能通过再签发复活。
    let loaded = store
        .load_device_credential(ACCOUNT, DEVICE)
        .expect("load")
        .expect("present");
    assert!(loaded.revoked);
    assert_eq!(
        store
            .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
            .unwrap_err()
            .code(),
        "INVALID_STATE_TRANSITION"
    );

    // 撤销第二台设备：版本单调 +1；第一台仍保持原版本。
    let second = store
        .revoke_device(ACCOUNT, DEVICE_TWO, ISSUED_AT + 20)
        .expect("revoke second");
    assert_eq!(second.revocation_generation, 2);
    assert_eq!(store.device_revocation_generation(ACCOUNT).expect("epoch"), 2);
    let set = store.revoked_devices(ACCOUNT).expect("revocation set");
    assert_eq!(set.account_id, ACCOUNT);
    assert_eq!(set.generation, 2);
    assert_eq!(set.revoked.len(), 2);
    assert_eq!(set.revoked[0].device_id, DEVICE);
    assert_eq!(set.revoked[0].revocation_generation, 1);
    drop(store);

    // 重开：撤销状态与版本仍在（不可静默复活）。
    let mut store = fixture.open();
    assert_eq!(store.device_revocation_generation(ACCOUNT).expect("epoch"), 2);
    let loaded = store
        .load_device_credential(ACCOUNT, DEVICE)
        .expect("load")
        .expect("present");
    assert!(loaded.revoked);
    assert_eq!(
        store
            .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
            .unwrap_err()
            .code(),
        "INVALID_STATE_TRANSITION"
    );
    // 未知设备也允许登记撤销（deny-list 可以先于凭据到达）。
    let unknown = store
        .revoke_device(ACCOUNT, "dev-unknown", ISSUED_AT + 30)
        .expect("revoke unknown device");
    assert_eq!(unknown.revocation_generation, 3);
}

#[test]
fn revocations_are_scoped_per_account() {
    let fixture = Fixture::new("scope");
    let mut store = fixture.open();
    store
        .save_device_credential(credential(ACCOUNT, DEVICE, 0x21))
        .expect("save");
    // 另一个账户撤销同名设备：不影响本账户。
    let foreign = store
        .revoke_device(OTHER_ACCOUNT, DEVICE, ISSUED_AT + 10)
        .expect("foreign revoke");
    assert_eq!(foreign.revocation_generation, 1);
    assert_eq!(store.device_revocation_generation(ACCOUNT).expect("epoch"), 0);
    assert!(store.revoked_devices(ACCOUNT).expect("set").revoked.is_empty());
    let loaded = store
        .load_device_credential(ACCOUNT, DEVICE)
        .expect("load")
        .expect("present");
    assert!(!loaded.revoked, "另一个账户的撤销不能影响本账户设备");
    assert!(store
        .load_device_credential(OTHER_ACCOUNT, DEVICE)
        .expect("cross account load")
        .is_none());
    assert!(store
        .list_device_credentials(OTHER_ACCOUNT)
        .expect("cross account list")
        .is_empty());
}

#[test]
fn bounds_and_invalid_inputs_are_rejected() {
    let fixture = Fixture::new("bounds");
    let mut store = fixture.open();

    for index in 0..MAX_DEVICE_CREDENTIALS {
        let device = format!("dev-{index}");
        store
            .save_device_credential(credential(ACCOUNT, &device, 0x30 + index as u8))
            .expect("save within limit");
    }
    assert_eq!(
        store
            .save_device_credential(credential(ACCOUNT, "dev-overflow", 0x7f))
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );

    // 非法输入。
    let mut empty_account = credential(ACCOUNT, DEVICE, 0x21);
    empty_account.account_id = String::new();
    assert_eq!(
        store.save_device_credential(empty_account).unwrap_err().code(),
        "INVALID_INPUT"
    );
    let mut bad_id = credential(ACCOUNT, "dev 1", 0x21);
    bad_id.device_id = "dev 1".to_owned();
    assert_eq!(
        store.save_device_credential(bad_id).unwrap_err().code(),
        "INVALID_INPUT"
    );
    let mut unknown_capability = credential(OTHER_ACCOUNT, DEVICE, 0x21);
    unknown_capability.capability_mask = 0b1000_0000;
    assert_eq!(
        store
            .save_device_credential(unknown_capability)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
    let mut zero_issued = credential(OTHER_ACCOUNT, DEVICE, 0x21);
    zero_issued.issued_at_ms = 0;
    assert_eq!(
        store.save_device_credential(zero_issued).unwrap_err().code(),
        "INVALID_INPUT"
    );
    let mut inverted = credential(OTHER_ACCOUNT, DEVICE, 0x21);
    inverted.expires_at_ms = inverted.issued_at_ms;
    assert_eq!(
        store.save_device_credential(inverted).unwrap_err().code(),
        "INVALID_INPUT"
    );
    assert_eq!(
        store
            .revoke_device(ACCOUNT, DEVICE, -1)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );

    // 撤销集合上限：只用未知设备填充，验证有界拒绝。
    for index in 0..MAX_DEVICE_REVOCATIONS {
        let device = format!("unknown-{index}");
        store
            .revoke_device(ACCOUNT, &device, ISSUED_AT + i64::from(index))
            .expect("revoke within limit");
    }
    assert_eq!(
        store
            .revoke_device(ACCOUNT, "unknown-overflow", ISSUED_AT)
            .unwrap_err()
            .code(),
        "INVALID_INPUT"
    );
}