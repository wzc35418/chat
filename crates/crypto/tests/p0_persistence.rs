use chat_crypto::persistence::DurableDevice;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    dir: PathBuf,
    alice: PathBuf,
    bob: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("chat-p0-crash-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let alice = dir.join("alice.db");
        let bob = dir.join("bob.db");
        let mut a = DurableDevice::create(&alice, "Alice").unwrap();
        let mut b = DurableDevice::create(&bob, "Bob").unwrap();
        let package = b.key_package().unwrap();
        let welcome = a.invite(&package, b.name(), b.public_key()).unwrap();
        b.join(&welcome, a.group_id().unwrap()).unwrap();
        drop(a);
        drop(b);
        Self { dir, alice, bob }
    }
    fn alice(&self) -> DurableDevice {
        DurableDevice::open(&self.alice).unwrap()
    }
    fn bob(&self) -> DurableDevice {
        DurableDevice::open(&self.bob).unwrap()
    }
    fn rows(&self, db: &Path, table: &str) -> i64 {
        let connection = rusqlite::Connection::open(db).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn crash(
        &self,
        mode: &str,
        db: &Path,
        id: &str,
        payload: &str,
        sender: &str,
        checkpoint: &str,
    ) {
        let output = Command::new(env!("CARGO_BIN_EXE_p0-crash-worker"))
            .args([mode, db.to_str().unwrap(), id, payload, sender, checkpoint])
            .output()
            .unwrap();
        assert!(!output.status.success(), "worker unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "P0_CRASH_CHECKPOINT={}\n",
                match checkpoint {
                    "draft" => "AfterDraft",
                    "mls" => "AfterMls",
                    "attempt" => "AfterAttempt",
                    "commit" => "AfterCommit",
                    _ => unreachable!(),
                }
            )),
            "worker did not reach checkpoint: {stderr}"
        );
    }
}

#[test]
fn c01_intent_and_prepare_job_survive_only_together() {
    for point in ["draft", "commit"] {
        let fixture = Fixture::new();
        let draft = format!("draft-{point}");
        let operation = format!("operation-{point}");
        let text = format!("durable draft {point}");
        fixture.crash("intent", &fixture.alice, &draft, &text, &operation, point);
        let mut alice = fixture.alice();
        if point == "draft" {
            assert!(alice.intent(&draft).unwrap().is_none());
            assert!(alice.waiting_intents().unwrap().is_empty());
            assert_eq!(fixture.rows(&fixture.alice, "p0_intents"), 0);
            assert_eq!(fixture.rows(&fixture.alice, "p0_prepare_jobs"), 0);
        } else {
            let intent = alice.intent(&draft).unwrap().unwrap();
            assert_eq!(intent.state, "queued");
            assert_eq!(intent.operation_id, operation);
            assert_eq!(alice.waiting_intents().unwrap(), vec![draft.clone()]);
            assert_eq!(fixture.rows(&fixture.alice, "p0_intents"), 1);
            assert_eq!(fixture.rows(&fixture.alice, "p0_prepare_jobs"), 1);
        }
        alice.persist_intent(&draft, &operation, &text).unwrap();
        alice.persist_intent(&draft, &operation, &text).unwrap();
        assert!(alice.persist_intent(&draft, &operation, "changed").is_err());
        assert!(
            alice
                .persist_intent("other-draft", &operation, &text)
                .is_err()
        );
        let submission = format!("submission-{point}");
        let cipher = alice.prepare_intent(&draft, &submission).unwrap();
        assert_eq!(alice.prepare_intent(&draft, &submission).unwrap(), cipher);
        assert!(
            alice
                .prepare_intent(&draft, "different-submission")
                .is_err()
        );
        let prepared = alice.intent(&draft).unwrap().unwrap();
        assert_eq!(prepared.state, "prepared");
        assert_eq!(prepared.submission_id.as_deref(), Some(submission.as_str()));
        assert!(alice.waiting_intents().unwrap().is_empty());
        assert_eq!(
            fixture
                .bob()
                .receive(&submission, &cipher, "Alice")
                .unwrap(),
            text
        );
        println!("C01 {point}: draft and prepare job atomic, T2 consumes same intent once");
    }
}

#[test]
fn intent_prepare_crash_reconciles_job_state_with_mls_and_outbox() {
    for point in ["mls", "attempt", "commit"] {
        let fixture = Fixture::new();
        let draft = format!("prepare-draft-{point}");
        let submission = format!("prepare-submission-{point}");
        let mut alice = fixture.alice();
        alice
            .persist_intent(&draft, &format!("op-{point}"), "intent message")
            .unwrap();
        drop(alice);
        fixture.crash(
            "prepare-intent",
            &fixture.alice,
            &draft,
            &submission,
            "unused",
            point,
        );
        let mut alice = fixture.alice();
        let before = alice.intent(&draft).unwrap().unwrap();
        if point == "commit" {
            assert_eq!(before.state, "prepared");
            assert_eq!(alice.pending().unwrap().len(), 1);
            assert!(alice.waiting_intents().unwrap().is_empty());
        } else {
            assert_eq!(before.state, "queued");
            assert!(alice.pending().unwrap().is_empty());
            assert_eq!(alice.waiting_intents().unwrap(), vec![draft.clone()]);
        }
        assert_eq!(
            fixture.rows(&fixture.alice, "p0_attempts"),
            fixture.rows(&fixture.alice, "p0_outbox")
        );
        let cipher = alice.prepare_intent(&draft, &submission).unwrap();
        assert_eq!(alice.prepare_intent(&draft, &submission).unwrap(), cipher);
        assert_eq!(
            fixture
                .bob()
                .receive(&submission, &cipher, "Alice")
                .unwrap(),
            "intent message"
        );
        println!("T1→T2 {point}: intent, job, MLS, attempt and outbox recover consistently");
    }
}

#[test]
fn prepare_job_insert_error_rolls_back_draft() {
    let fixture = Fixture::new();
    let connection = rusqlite::Connection::open(&fixture.alice).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_prepare_job BEFORE INSERT ON p0_prepare_jobs BEGIN SELECT RAISE(ABORT, 'injected prepare failure'); END;").unwrap();
    assert!(
        fixture
            .alice()
            .persist_intent("draft-error", "op-error", "body")
            .is_err()
    );
    assert_eq!(fixture.rows(&fixture.alice, "p0_intents"), 0);
    assert_eq!(fixture.rows(&fixture.alice, "p0_prepare_jobs"), 0);
    connection
        .execute_batch("DROP TRIGGER reject_prepare_job")
        .unwrap();
    let mut alice = fixture.alice();
    alice
        .persist_intent("draft-error", "op-error", "body")
        .unwrap();
    assert_eq!(alice.waiting_intents().unwrap(), vec!["draft-error"]);
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn process_abort_before_and_after_send_commit_recovers_exact_outbox() {
    for point in ["mls", "attempt", "commit"] {
        let fixture = Fixture::new();
        let id = format!("send-{point}");
        let body = format!("真实事务消息 {point} 👋");
        fixture.crash("send", &fixture.alice, &id, &body, "Alice", point);
        let mut alice = fixture.alice();
        let pending = alice.pending().unwrap();
        if point == "commit" {
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].0, id);
            assert_eq!(alice.prepare(&id, &body).unwrap(), pending[0].1);
            assert!(alice.prepare(&id, "different text").is_err());
        } else {
            assert!(pending.is_empty(), "partial attempt/outbox after {point}");
        }
        assert_eq!(
            fixture.rows(&fixture.alice, "p0_attempts"),
            pending.len() as i64
        );
        assert_eq!(
            fixture.rows(&fixture.alice, "p0_outbox"),
            pending.len() as i64
        );
        let cipher = alice.prepare(&id, &body).unwrap();
        assert_eq!(alice.pending().unwrap(), vec![(id.clone(), cipher.clone())]);
        assert_eq!(fixture.bob().receive(&id, &cipher, "Alice").unwrap(), body);
        let next = alice
            .prepare(&format!("next-{point}"), "next generation")
            .unwrap();
        assert_eq!(
            fixture
                .bob()
                .receive(&format!("next-{point}"), &next, "Alice")
                .unwrap(),
            "next generation"
        );
        println!(
            "send {point}: recovered {} complete attempt with consistent MLS state",
            pending.len()
        );
    }
}

#[test]
fn rejected_cipher_rolls_back_provider_and_duplicate_receive_is_idempotent() {
    let fixture = Fixture::new();
    let cipher = fixture
        .alice()
        .prepare("rx-1", "receive transaction")
        .unwrap();
    let mut corrupted = cipher.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(fixture.bob().receive("rx-1", &corrupted, "Alice").is_err());
    let mut bob = fixture.bob();
    assert_eq!(
        bob.receive("rx-1", &cipher, "Alice").unwrap(),
        "receive transaction"
    );
    assert_eq!(
        fixture.bob().receive("rx-1", &cipher, "Alice").unwrap(),
        "receive transaction"
    );
    assert!(fixture.bob().receive("rx-1", &corrupted, "Alice").is_err());
    assert!(fixture.bob().receive("rx-1", &cipher, "Mallory").is_err());
    println!(
        "Rejected MLS input rolled back; valid ciphertext decrypts after reopen; duplicate inbox idempotent"
    );
}

#[test]
fn sql_constraint_failure_after_mls_write_rolls_back_send_state() {
    let fixture = Fixture::new();
    let connection = rusqlite::Connection::open(&fixture.alice).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_outbox BEFORE INSERT ON p0_outbox BEGIN SELECT RAISE(ABORT, 'injected outbox failure'); END;").unwrap();
    assert!(
        fixture
            .alice()
            .prepare("failed-attempt", "recoverable message")
            .is_err()
    );
    connection
        .execute_batch("DROP TRIGGER reject_outbox")
        .unwrap();
    let mut alice = fixture.alice();
    assert!(alice.pending().unwrap().is_empty());
    let cipher = alice
        .prepare("failed-attempt", "recoverable message")
        .unwrap();
    assert_eq!(
        fixture
            .bob()
            .receive("failed-attempt", &cipher, "Alice")
            .unwrap(),
        "recoverable message"
    );
    println!("SQLite outbox constraint failure rolled back OpenMLS sender state and attempt");
}

#[test]
fn process_abort_during_receive_recovers_inbox_and_secret() {
    for point in ["mls", "attempt", "commit"] {
        let fixture = Fixture::new();
        let id = format!("rx-{point}");
        let cipher = fixture.alice().prepare(&id, "durable receive").unwrap();
        let payload = fixture.dir.join("cipher.bin");
        fs::write(&payload, &cipher).unwrap();
        fixture.crash(
            "receive",
            &fixture.bob,
            &id,
            payload.to_str().unwrap(),
            "Alice",
            point,
        );
        assert_eq!(
            fixture.bob().receive(&id, &cipher, "Alice").unwrap(),
            "durable receive"
        );
        assert_eq!(fixture.rows(&fixture.bob, "p0_inbox"), 1);
        assert_eq!(
            fixture.bob().receive(&id, &cipher, "Alice").unwrap(),
            "durable receive"
        );
        println!("C06 local receive {point}: recovered with one inbox row");
    }
}
