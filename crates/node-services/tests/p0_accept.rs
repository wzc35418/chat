use chat_crypto::persistence::DurableDevice;
use chat_node_services::p0_accept::{P0NodeStore, verify_receipt};
use rusqlite::OptionalExtension;
use std::{fs, path::PathBuf, process::Command};

struct Fixture {
    dir: PathBuf,
    alice: PathBuf,
    bob: PathBuf,
    node: PathBuf,
    group: String,
    signing_seed: [u8; 32],
    seed_file: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("chat-p0-node-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let alice = dir.join("alice.db");
        let bob = dir.join("bob.db");
        let node = dir.join("node.db");
        let seed_file = dir.join("node-seed.bin");
        let mut signing_seed = [0_u8; 32];
        signing_seed[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        signing_seed[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        fs::write(&seed_file, signing_seed).unwrap();
        let mut a = DurableDevice::create(&alice, "Alice").unwrap();
        let mut b = DurableDevice::create(&bob, "Bob").unwrap();
        let welcome = a
            .invite(&b.key_package().unwrap(), b.name(), b.public_key())
            .unwrap();
        b.join(&welcome, a.group_id().unwrap()).unwrap();
        let group: String = a
            .group_id()
            .unwrap()
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect();
        P0NodeStore::open(&node, &signing_seed)
            .unwrap()
            .init_group(&group)
            .unwrap();
        Self {
            dir,
            alice,
            bob,
            node,
            group,
            signing_seed,
            seed_file,
        }
    }
    fn node(&self) -> P0NodeStore {
        P0NodeStore::open(&self.node, &self.signing_seed).unwrap()
    }
    fn cipher(&self, draft: &str, submission: &str, text: &str) -> Vec<u8> {
        let mut alice = DurableDevice::open(&self.alice).unwrap();
        alice
            .persist_intent(draft, &format!("op-{draft}"), text)
            .unwrap();
        let cipher = alice.prepare_intent(draft, submission).unwrap();
        assert_eq!(
            alice
                .pending()
                .unwrap()
                .iter()
                .find(|(id, _)| id == submission)
                .unwrap()
                .1,
            cipher
        );
        cipher
    }
    fn crash(&self, submission: &str, cipher: &[u8], point: &str) {
        let file = self.dir.join("cipher.bin");
        fs::write(&file, cipher).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_p0-node-crash-worker"))
            .args([
                self.node.to_str().unwrap(),
                &self.group,
                submission,
                file.to_str().unwrap(),
                point,
                self.seed_file.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "child unexpectedly succeeded");
        let expected = match point {
            "log" => "AfterLog",
            "before-commit" => "BeforeCommit",
            "after-commit" => "AfterCommit",
            _ => unreachable!(),
        };
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains(&format!("P0_NODE_CRASH_CHECKPOINT={expected}")),
            "child missed checkpoint: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn integrity(&self) {
        let connection = rusqlite::Connection::open(&self.node).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            connection
                .query_row("PRAGMA foreign_key_check", [], |r| r.get::<_, String>(0))
                .optional()
                .unwrap(),
            None
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn c05_committed_node_result_survives_lost_response_and_client_retries_same_cipher() {
    let f = Fixture::new();
    let cipher = f.cipher("draft1", "submission1", "MLS node acceptance");
    f.crash("submission1", &cipher, "after-commit");
    f.integrity();
    let saved = f.node().receipt(&f.group, "submission1").unwrap().unwrap();
    assert_eq!(saved.seq, 1);
    let pinned_key = f.node().node_public_key();
    let wrong_seed = [9_u8; 32];
    assert!(P0NodeStore::open(&f.node, &wrong_seed).is_err());
    verify_receipt(&pinned_key, &f.group, "submission1", &cipher, &saved).unwrap();
    let mut bad_signature = saved.clone();
    let first_byte = u8::from_str_radix(&bad_signature.signature[..2], 16).unwrap() ^ 1;
    bad_signature
        .signature
        .replace_range(0..2, &format!("{first_byte:02x}"));
    assert!(
        verify_receipt(
            &pinned_key,
            &f.group,
            "submission1",
            &cipher,
            &bad_signature
        )
        .is_err()
    );
    let mut non_ascii_signature = saved.clone();
    non_ascii_signature.signature = "é".repeat(64);
    assert!(
        verify_receipt(
            &pinned_key,
            &f.group,
            "submission1",
            &cipher,
            &non_ascii_signature,
        )
        .is_err()
    );
    assert!(verify_receipt(&[9_u8; 32], &f.group, "submission1", &cipher, &saved).is_err());
    let mut wrong_sequence = saved.clone();
    wrong_sequence.seq += 1;
    assert!(
        verify_receipt(
            &pinned_key,
            &f.group,
            "submission1",
            &cipher,
            &wrong_sequence
        )
        .is_err()
    );
    assert_eq!(f.node().audit(&f.group).unwrap(), 1);
    let retry = DurableDevice::open(&f.alice)
        .unwrap()
        .prepare_intent("draft1", "submission1")
        .unwrap();
    assert_eq!(retry, cipher, "client must reuse committed ciphertext");
    let reply = f.node().accept(&f.group, "submission1", &retry).unwrap();
    assert_eq!(reply, saved, "node must return original durable receipt");
    let mut altered = retry.clone();
    *altered.last_mut().unwrap() ^= 1;
    assert!(f.node().accept(&f.group, "submission1", &altered).is_err());
    assert!(verify_receipt(&pinned_key, &f.group, "submission1", &altered, &saved).is_err());
    assert_eq!(f.node().audit(&f.group).unwrap(), 1);
    assert_eq!(
        DurableDevice::open(&f.bob)
            .unwrap()
            .receive("submission1", &cipher, "Alice")
            .unwrap(),
        "MLS node acceptance"
    );
    let second = f.cipher("draft2", "submission2", "second message");
    let next = f.node().accept(&f.group, "submission2", &second).unwrap();
    assert_eq!(next.seq, 2);
    assert_eq!(next.previous_hash, saved.log_hash);
    assert_eq!(f.node().audit(&f.group).unwrap(), 2);
    println!(
        "C05: committed result replay returned original seq/hash receipt and fixed MLS ciphertext"
    );
}

#[test]
fn aborted_node_transaction_never_exposes_half_log_or_delivery_job() {
    for point in ["log", "before-commit"] {
        let f = Fixture::new();
        let cipher = f.cipher("draft1", "submission1", "atomic log job");
        f.crash("submission1", &cipher, point);
        f.integrity();
        assert!(f.node().receipt(&f.group, "submission1").unwrap().is_none());
        assert_eq!(f.node().audit(&f.group).unwrap(), 0);
        let receipt = f.node().accept(&f.group, "submission1", &cipher).unwrap();
        assert_eq!(receipt.seq, 1);
        assert_eq!(f.node().audit(&f.group).unwrap(), 1);
        println!("node {point}: pre-commit log, head and delivery job rolled back together");
    }
}

#[test]
fn concurrent_writers_allocate_unique_contiguous_sequence() {
    let f = Fixture::new();
    let mut handles = Vec::new();
    for i in 0..8 {
        let db = f.node.clone();
        let group = f.group.clone();
        let seed = f.signing_seed;
        handles.push(std::thread::spawn(move || {
            P0NodeStore::open(&db, &seed)
                .unwrap()
                .accept(
                    &group,
                    &format!("submission-{i}"),
                    format!("cipher-{i}").as_bytes(),
                )
                .unwrap()
                .seq
        }));
    }
    let mut sequences: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    sequences.sort();
    assert_eq!(sequences, (1..=8).collect::<Vec<_>>());
    assert_eq!(f.node().audit(&f.group).unwrap(), 8);
}

#[test]
fn stored_receipt_tampering_is_rejected_after_reopen() {
    let f = Fixture::new();
    let cipher = f.cipher("draft1", "submission1", "signed receipt tamper");
    f.node().accept(&f.group, "submission1", &cipher).unwrap();
    let connection = rusqlite::Connection::open(&f.node).unwrap();
    connection
        .execute(
            "UPDATE p0_log SET receipt=?1 WHERE group_id=?2 AND submission_id='submission1'",
            rusqlite::params![br#"{"signature":"forged"}"#, &f.group],
        )
        .unwrap();
    drop(connection);
    assert!(f.node().receipt(&f.group, "submission1").is_err());
    assert!(f.node().accept(&f.group, "submission1", &cipher).is_err());
    assert!(f.node().audit(&f.group).is_err());
}
