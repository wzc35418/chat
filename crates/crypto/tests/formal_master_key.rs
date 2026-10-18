//! P0-04 主密钥派生/加载，以及与正式存储 open_store 的端到端用例。

use chat_crypto::key_store::{
    KdfParams, MAX_M_COST_KIB, MAX_PARALLELISM, MAX_PASSPHRASE_BYTES, MAX_SALT_BYTES, MAX_T_COST,
    MIN_M_COST_KIB, derive_master_key, load_master_key_file, new_salt,
};
use chat_storage::formal::{
    MessageContent, MessageOrigin, ProtectedStorageKey, StoreOptions, ValidatedSendRequest,
    WriteScope, commit_transaction, open_store, persist_send_intent,
};
use std::fs;
use std::path::PathBuf;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("chat-key-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// 测试用低成本参数（仍在有界范围内）。
fn fast_params(passphrase: &[u8]) -> KdfParams {
    let _ = passphrase;
    KdfParams::with_salt(MIN_M_COST_KIB, 1, 1, new_salt().unwrap()).unwrap()
}

fn random_bytes(length: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(length);
    while bytes.len() < length {
        bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    bytes.truncate(length);
    bytes
}

#[test]
fn argon2id_derivation_is_deterministic_and_bound_to_salt_and_passphrase() {
    let params = fast_params(b"correct horse");
    let first = derive_master_key(b"correct horse", &params).unwrap();
    let second = derive_master_key(b"correct horse", &params).unwrap();
    assert!(first.key.equals(&second.key));
    assert_eq!(first.provenance, params_to_provenance(&params));
    assert_eq!(first.provenance.kdf_version(), 2);

    let other_salt = KdfParams::with_salt(
        params.m_cost_kib,
        params.t_cost,
        params.parallelism,
        new_salt().unwrap(),
    )
    .unwrap();
    let different_salt = derive_master_key(b"correct horse", &other_salt).unwrap();
    assert!(!first.key.equals(&different_salt.key));

    let different_passphrase = derive_master_key(b"wrong horse", &params).unwrap();
    assert!(!first.key.equals(&different_passphrase.key));
}

fn params_to_provenance(params: &KdfParams) -> chat_storage::formal::KeyProvenance {
    chat_storage::formal::KeyProvenance::Argon2id {
        m_cost_kib: params.m_cost_kib,
        t_cost: params.t_cost,
        parallelism: params.parallelism,
        salt: params.salt.clone(),
    }
}

#[test]
fn kdf_bounds_reject_malformed_parameters() {
    let salt = new_salt().unwrap();
    let build = |m: u32, t: u32, p: u32, salt: Vec<u8>| KdfParams::with_salt(m, t, p, salt);

    assert!(build(MIN_M_COST_KIB - 1, 1, 1, salt.clone()).is_err());
    assert!(build(MAX_M_COST_KIB + 1, 1, 1, salt.clone()).is_err());
    assert!(build(MIN_M_COST_KIB, 0, 1, salt.clone()).is_err());
    assert!(build(MIN_M_COST_KIB, MAX_T_COST + 1, 1, salt.clone()).is_err());
    assert!(build(MIN_M_COST_KIB, 1, 0, salt.clone()).is_err());
    assert!(build(MIN_M_COST_KIB, 1, MAX_PARALLELISM + 1, salt.clone()).is_err());
    assert!(build(MIN_M_COST_KIB, 1, 1, vec![0u8; 15]).is_err());
    assert!(build(MIN_M_COST_KIB, 1, 1, vec![0u8; MAX_SALT_BYTES + 1]).is_err());
    assert!(build(MIN_M_COST_KIB, 1, 1, salt).is_ok());

    let params = fast_params(b"x");
    assert!(derive_master_key(b"", &params).is_err());
    let oversized = vec![b'x'; MAX_PASSPHRASE_BYTES + 1];
    assert!(derive_master_key(&oversized, &params).is_err());
    let maximum = vec![b'x'; MAX_PASSPHRASE_BYTES];
    assert!(derive_master_key(&maximum, &params).is_ok());
}

#[test]
fn key_file_loading_is_length_bounded() {
    let dir = TempDir::new();
    let path = dir.path.join("store.key");
    let bytes = random_bytes(32);

    fs::write(&path, &bytes).unwrap();
    let loaded = load_master_key_file(&path).unwrap();
    assert!(loaded.equals(&ProtectedStorageKey::from_bytes(exact32(&bytes))));

    fs::write(&path, &bytes[..31]).unwrap();
    assert!(load_master_key_file(&path).is_err());
    fs::write(&path, [&bytes[..], &[0u8]].concat()).unwrap();
    assert!(load_master_key_file(&path).is_err());
    assert!(load_master_key_file(&dir.path.join("missing.key")).is_err());
    assert!(load_master_key_file(&dir.path).is_err(), "directory is not a key file");
    fs::remove_file(&path).unwrap();
}

fn exact32(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes[..32]);
    out
}

#[test]
fn derived_key_opens_the_formal_store_and_wrong_passphrase_is_rejected() {
    let dir = TempDir::new();
    let path = dir.path.join("chat.db");
    let params = fast_params(b"hunter2");
    let derived = derive_master_key(b"hunter2", &params).unwrap();
    let draft_id;
    {
        let mut store = open_store(
            StoreOptions::new(&path, derived.provenance.clone()),
            derived.key,
        )
        .unwrap();
        // 迁移版本随 formal store 演进而增加（本轮新增 0003 identity）。
        assert!(store.schema_version() >= 2);
        store
            .connection()
            .execute(
                "INSERT INTO accounts (account_id, public_key, protected_profile) VALUES ('account-1', X'00', X'00')",
                [],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO conversations (conversation_id, kind, coordinator_id, protected_metadata, state)
                 VALUES ('conv-key', 'direct', 'node-1', X'00', 'active')",
                [],
            )
            .unwrap();
        let mut transaction = store.begin_transaction(WriteScope::intent("conv-key")).unwrap();
        let record = persist_send_intent(
            &mut transaction,
            ValidatedSendRequest {
                conversation_id: "conv-key".to_owned(),
                client_operation_id: "op-key".to_owned(),
                content: MessageContent::text("主密钥派生正文").unwrap(),
                reply_to: None,
                origin: MessageOrigin::UserInput,
                created_at: 1_700_000_000_000,
            },
        )
        .unwrap();
        commit_transaction(transaction).unwrap();
        draft_id = record.draft_id;
    }

    // 相同口令与参数重开：读到同一草稿。
    let reopened = derive_master_key(b"hunter2", &params).unwrap();
    let store = open_store(
        StoreOptions::new(&path, reopened.provenance.clone()),
        reopened.key,
    )
    .unwrap();
    let draft = store.read_draft(&draft_id).unwrap().unwrap();
    assert_eq!(
        draft.content,
        MessageContent::text("主密钥派生正文").unwrap()
    );
    drop(store);

    // 不同口令派生出的密钥必须被拒绝，且不覆盖数据。
    let wrong = derive_master_key(b"hunter3", &params).unwrap();
    let error = match open_store(StoreOptions::new(&path, wrong.provenance), wrong.key) {
        Ok(_) => panic!("wrong passphrase must not open the store"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STATE_CORRUPTED");
    // 数据没有被覆盖：用正确口令再次派生仍能读到同一草稿。
    let again = derive_master_key(b"hunter2", &params).unwrap();
    let store = open_store(StoreOptions::new(&path, again.provenance), again.key).unwrap();
    assert!(store.read_draft(&draft_id).unwrap().is_some());
}

#[test]
fn same_key_with_different_provenance_is_rejected() {
    let dir = TempDir::new();
    let path = dir.path.join("chat.db");
    let params = fast_params(b"seed");
    let derived = derive_master_key(b"seed", &params).unwrap();
    let _store = open_store(
        StoreOptions::new(&path, derived.provenance.clone()),
        derived.key,
    )
    .unwrap();
    drop(_store);

    // 同一口令但换一套参数：派生出的密钥不同，且来源记录必须不一致，必须拒绝。
    let other_params = KdfParams::with_salt(
        params.m_cost_kib,
        params.t_cost + 1,
        params.parallelism,
        new_salt().unwrap(),
    )
    .unwrap();
    let other = derive_master_key(b"seed", &other_params).unwrap();
    let error = match open_store(StoreOptions::new(&path, other.provenance), other.key) {
        Ok(_) => panic!("mismatched provenance must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "STATE_CORRUPTED");
}
