//! 只供正式存储崩溃集成测试使用的子进程。
//!
//! 在指定断点向 stderr 输出标记后调用 std::process::abort()，父进程据此验证
//! “已提交完整状态或什么都没有”，不使用任何模拟成功路径。
//!
//! 用法：
//!   storage-crash-worker intent  <db> <key-hex> <checkpoint> <conversation> <operation> <text>
//!   storage-crash-worker attempt <db> <key-hex> <checkpoint> <conversation> <draft> <submission> <epoch> <cipher-hex> <secret>
use chat_storage::formal::{
    KeyProvenance, PreparedWrite, ProtectedStorageKey, StoreOptions, ValidatedSendRequest,
    WriteCheckpoint, WriteScope, commit_transaction, open_store, persist_prepared_attempt_with_checkpoint,
    persist_send_intent_with_checkpoint,
};
use chat_storage::formal::{CryptoRecordChange, MessageContent, MessageOrigin};
use std::env;
use std::path::PathBuf;

fn main() {
    let arguments: Vec<String> = env::args().collect();
    if arguments.len() < 6 {
        panic!("storage-crash-worker: not enough arguments");
    }
    let mode = arguments[1].as_str();
    let path = PathBuf::from(&arguments[2]);
    let key = ProtectedStorageKey::try_from_slice(&decode_hex(&arguments[3])).unwrap();
    let target = match arguments[4].as_str() {
        "draft" => WriteCheckpoint::AfterDraft,
        "prepare-job" => WriteCheckpoint::AfterPrepareJob,
        "attempt" => WriteCheckpoint::AfterAttempt,
        "mls" => WriteCheckpoint::AfterMlsRecords,
        "outbox" => WriteCheckpoint::AfterOutbox,
        other => panic!("unknown checkpoint: {other}"),
    };
    let conversation = arguments[5].clone();
    let mut store =
        open_store(StoreOptions::new(path, KeyProvenance::Raw32), key).expect("open formal store");
    let mut checkpoint = |stage: WriteCheckpoint| {
        if stage == target {
            eprintln!("STORAGE_CRASH_CHECKPOINT={stage:?}");
            std::process::abort();
        }
        Ok(())
    };
    match mode {
        "intent" => {
            let operation = arguments[6].clone();
            let text = arguments[7].clone();
            let request = ValidatedSendRequest {
                conversation_id: conversation.clone(),
                client_operation_id: operation,
                content: MessageContent::text(text).unwrap(),
                reply_to: None,
                origin: MessageOrigin::UserInput,
                created_at: 1_700_000_000_000,
            };
            let mut transaction = store
                .begin_transaction(WriteScope::intent(conversation))
                .expect("begin intent transaction");
            persist_send_intent_with_checkpoint(&mut transaction, request, &mut checkpoint)
                .expect("persist intent");
            commit_transaction(transaction).expect("commit intent");
        }
        "attempt" => {
            let draft_id = arguments[6].clone();
            let submission_id = arguments[7].clone();
            let epoch: u64 = arguments[8].parse().expect("mls epoch");
            let ciphertext = decode_hex(&arguments[9]);
            let secret = arguments[10].clone().into_bytes();
            let draft = {
                let read = store.read_draft(&draft_id).expect("read draft");
                read.expect("draft must exist")
            };
            let prepared = PreparedWrite {
                draft_id,
                conversation_id: conversation.clone(),
                submission_id,
                mls_epoch: epoch,
                immutable_ciphertext: ciphertext,
                request_hash: draft.request_hash,
                crypto_changes: vec![CryptoRecordChange::new(
                    b"mls/group-state".to_vec(),
                    0,
                    secret,
                )],
            };
            let mut transaction = store
                .begin_transaction(WriteScope::attempt(conversation))
                .expect("begin attempt transaction");
            let record = persist_prepared_attempt_with_checkpoint(&mut transaction, prepared, &mut checkpoint)
                .expect("persist attempt");
            assert!(!record.submission_id.is_empty());
            commit_transaction(transaction).expect("commit attempt");
        }
        other => panic!("unknown mode: {other}"),
    }
    panic!("crash checkpoint {target:?} was not reached");
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert!(value.len() % 2 == 0, "hex value must have an even length");
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for index in (0..value.len()).step_by(2) {
        let byte = u8::from_str_radix(&value[index..index + 2], 16).expect("valid hex");
        bytes.push(byte);
    }
    bytes
}
