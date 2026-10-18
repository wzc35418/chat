//! Isolated P0 durability experiment. Tables contain plaintext metadata and
//! unencrypted MLS secrets, so callers must use disposable test databases only.
use anyhow::{Result, ensure};
use openmls_sqlite_storage::{Codec, SqliteStorageProvider};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Default)]
pub struct JsonCodec;
impl Codec for JsonCodec {
    type Error = serde_json::Error;
    fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, Self::Error> {
        serde_json::to_vec(value)
    }
    fn from_slice<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Self::Error> {
        serde_json::from_slice(bytes)
    }
}

pub struct P0Store {
    pub connection: Connection,
}
impl P0Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let mut connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        SqliteStorageProvider::<JsonCodec, &mut Connection>::new(&mut connection)
            .run_migrations()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS p0_meta (key TEXT PRIMARY KEY, value BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS p0_attempts (
               submission_id TEXT PRIMARY KEY, cipher BLOB NOT NULL,
               digest BLOB NOT NULL, request_digest BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS p0_outbox (
               submission_id TEXT PRIMARY KEY REFERENCES p0_attempts(submission_id),
               state TEXT NOT NULL CHECK(state IN ('pending','complete')));
             CREATE TABLE IF NOT EXISTS p0_inbox (
               submission_id TEXT PRIMARY KEY, body BLOB NOT NULL,
               digest BLOB NOT NULL, sender TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS p0_intents (
               draft_id TEXT PRIMARY KEY, operation_id TEXT NOT NULL UNIQUE,
               body BLOB NOT NULL, request_digest BLOB NOT NULL,
               state TEXT NOT NULL CHECK(state IN ('queued','prepared')),
               submission_id TEXT UNIQUE REFERENCES p0_attempts(submission_id),
               CHECK((state='queued' AND submission_id IS NULL) OR
                     (state='prepared' AND submission_id IS NOT NULL)));
             CREATE TABLE IF NOT EXISTS p0_prepare_jobs (
               draft_id TEXT PRIMARY KEY REFERENCES p0_intents(draft_id),
               state TEXT NOT NULL CHECK(state IN ('waiting','complete')));",
        )?;
        Ok(Self { connection })
    }
    pub fn begin(&mut self) -> rusqlite::Result<rusqlite::Transaction<'_>> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
    }

    pub fn persist_intent(&mut self, draft_id: &str, operation_id: &str, text: &str) -> Result<()> {
        self.persist_intent_with_checkpoint(draft_id, operation_id, text, |_| {})
    }

    pub fn persist_intent_with_checkpoint(
        &mut self,
        draft_id: &str,
        operation_id: &str,
        text: &str,
        checkpoint: impl Fn(IntentCheckpoint),
    ) -> Result<()> {
        validate_id(draft_id)?;
        validate_id(operation_id)?;
        chat_protocol::wire::validate_text(text)?;
        let digest = Sha256::digest(text.as_bytes());
        let tx = self.begin()?;
        let existing = tx
            .query_row(
                "SELECT draft_id, operation_id, body, request_digest FROM p0_intents
             WHERE draft_id=?1 OR operation_id=?2",
                params![draft_id, operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((old_draft, old_operation, old_body, old_digest)) = existing {
            ensure!(
                old_draft == draft_id
                    && old_operation == operation_id
                    && old_body == text.as_bytes()
                    && old_digest == digest.as_slice(),
                "conflicting persisted intent"
            );
            let jobs: i64 = tx.query_row(
                "SELECT COUNT(*) FROM p0_prepare_jobs WHERE draft_id=?1",
                [draft_id],
                |r| r.get(0),
            )?;
            ensure!(jobs == 1, "persisted intent missing prepare job");
            return Ok(());
        }
        tx.execute(
            "INSERT INTO p0_intents VALUES (?1, ?2, ?3, ?4, 'queued', NULL)",
            params![draft_id, operation_id, text.as_bytes(), digest.as_slice()],
        )?;
        checkpoint(IntentCheckpoint::AfterDraft);
        tx.execute(
            "INSERT INTO p0_prepare_jobs VALUES (?1, 'waiting')",
            [draft_id],
        )?;
        tx.commit()?;
        checkpoint(IntentCheckpoint::AfterCommit);
        Ok(())
    }

    pub fn intent(&self, draft_id: &str) -> Result<Option<P0Intent>> {
        Ok(self
            .connection
            .query_row(
                "SELECT operation_id, body, state, submission_id FROM p0_intents WHERE draft_id=?1",
                [draft_id],
                |row| {
                    Ok(P0Intent {
                        draft_id: draft_id.to_owned(),
                        operation_id: row.get(0)?,
                        body: row.get(1)?,
                        state: row.get(2)?,
                        submission_id: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn waiting_intents(&self) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT i.draft_id FROM p0_intents i JOIN p0_prepare_jobs j USING(draft_id)
             WHERE i.state='queued' AND j.state='waiting' ORDER BY i.draft_id",
        )?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentCheckpoint {
    AfterDraft,
    AfterCommit,
}

#[derive(Debug, PartialEq, Eq)]
pub struct P0Intent {
    pub draft_id: String,
    pub operation_id: String,
    pub body: Vec<u8>,
    pub state: String,
    pub submission_id: Option<String>,
}

fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid P0 intent ID"
    );
    Ok(())
}
