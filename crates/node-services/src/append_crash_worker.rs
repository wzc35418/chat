//! Dedicated child process for the formal NODE-006 commit/response-loss experiment.
//!
//! 只用于证据：进程在事务中途 `abort()`，随后由测试重新打开同一个 SQLite 库，
//! 检查日志/head 是否整体回滚或整体提交（不允许半条日志）。
use chat_node_services::group::{AppendCheckpoint, SignedCandidate};
use chat_node_services::services::{AuthenticatedDevice, NodeAuthPolicy, NodeService, NodeServiceConfig};
use std::{env, fs, path::Path};

fn main() {
    let args: Vec<String> = env::args().collect();
    assert_eq!(
        args.len(),
        10,
        "db node_id seed_file group_id candidate_file checkpoint account_id device_id now"
    );
    let checkpoint = match args[6].as_str() {
        "log" => AppendCheckpoint::AfterLogInsert,
        "before-commit" => AppendCheckpoint::BeforeCommit,
        "after-commit" => AppendCheckpoint::AfterCommit,
        other => panic!("unknown checkpoint: {other}"),
    };
    let seed: [u8; 32] = fs::read(&args[3]).unwrap().try_into().unwrap();
    let candidate: SignedCandidate = serde_json::from_slice(&fs::read(&args[5]).unwrap()).unwrap();
    let actor = AuthenticatedDevice {
        account_id: args[7].clone(),
        device_id: args[8].clone(),
        credential_version: 1,
        challenge_id: "append-crash-worker".to_owned(),
    };
    let now: u64 = args[9].parse().unwrap();
    let config = NodeServiceConfig {
        node_id: args[2].clone(),
        signing_seed: seed,
        policy: NodeAuthPolicy::default(),
    };
    let mut node = NodeService::open(Path::new(&args[1]), config).unwrap();
    let receipt = node
        .append_group_entry_with_checkpoint(&args[4], &candidate, &actor, now, |stage| {
            if stage == checkpoint {
                eprintln!("NODE_APPEND_CRASH_CHECKPOINT={stage:?}");
                std::process::abort();
            }
        })
        .unwrap();
    panic!("node append crash checkpoint was not reached (seq={})", receipt.seq);
}