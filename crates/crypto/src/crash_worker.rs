//! Child process used only by the P0 crash integration test.
use chat_crypto::persistence::{Checkpoint, DurableDevice};
use chat_storage::store::{IntentCheckpoint, P0Store};
use std::{env, fs, path::Path};

fn main() {
    let args: Vec<String> = env::args().collect();
    assert_eq!(
        args.len(),
        7,
        "mode db submission payload sender checkpoint"
    );
    if args[1] == "intent" {
        let target = match args[6].as_str() {
            "draft" => IntentCheckpoint::AfterDraft,
            "commit" => IntentCheckpoint::AfterCommit,
            other => panic!("unknown intent checkpoint: {other}"),
        };
        let mut store = P0Store::open(Path::new(&args[2])).unwrap();
        store
            .persist_intent_with_checkpoint(&args[3], &args[5], &args[4], |stage| {
                if stage == target {
                    eprintln!("P0_CRASH_CHECKPOINT={stage:?}");
                    std::process::abort();
                }
            })
            .unwrap();
        panic!("intent crash checkpoint was not reached");
    }
    let target = match args[6].as_str() {
        "mls" => Checkpoint::AfterMls,
        "attempt" => Checkpoint::AfterAttempt,
        "commit" => Checkpoint::AfterCommit,
        other => panic!("unknown checkpoint: {other}"),
    };
    let mut device = DurableDevice::open(Path::new(&args[2])).unwrap();
    let crash = |stage| {
        if stage == target {
            eprintln!("P0_CRASH_CHECKPOINT={stage:?}");
            std::process::abort();
        }
    };
    match args[1].as_str() {
        "send" => {
            device
                .prepare_with_checkpoint(&args[3], &args[4], crash)
                .unwrap();
        }
        "prepare-intent" => {
            device
                .prepare_intent_with_checkpoint(&args[3], &args[4], crash)
                .unwrap();
        }
        "receive" => {
            let cipher = fs::read(&args[4]).unwrap();
            device
                .receive_with_checkpoint(&args[3], &cipher, &args[5], crash)
                .unwrap();
        }
        other => panic!("unknown mode: {other}"),
    }
    panic!("crash checkpoint was not reached");
}
