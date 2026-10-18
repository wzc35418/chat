//! Dedicated child process for the P0 node commit/response-loss experiment.
use chat_node_services::p0_accept::{AcceptCheckpoint, P0NodeStore};
use std::{env, fs, path::Path};

fn main() {
    let args: Vec<String> = env::args().collect();
    assert_eq!(
        args.len(),
        7,
        "db group submission ciphertext_file checkpoint seed_file"
    );
    let checkpoint = match args[5].as_str() {
        "log" => AcceptCheckpoint::AfterLog,
        "before-commit" => AcceptCheckpoint::BeforeCommit,
        "after-commit" => AcceptCheckpoint::AfterCommit,
        other => panic!("unknown checkpoint: {other}"),
    };
    let bytes = fs::read(&args[4]).unwrap();
    let seed: [u8; 32] = fs::read(&args[6]).unwrap().try_into().unwrap();
    let mut node = P0NodeStore::open(Path::new(&args[1]), &seed).unwrap();
    node.accept_with_checkpoint(&args[2], &args[3], &bytes, |stage| {
        if stage == checkpoint {
            eprintln!("P0_NODE_CRASH_CHECKPOINT={stage:?}");
            std::process::abort();
        }
    })
    .unwrap();
    panic!("node crash checkpoint was not reached");
}
