//! Disposable P0 node acceptance experiment. The caller supplies the node's
//! signing seed and pins its public key; key custody, authentication, HTTP,
//! authorization, mailbox fanout, and encrypted storage are not provided.
use anyhow::{Context, Result, ensure};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

const ZERO_HASH: [u8; 32] = [0; 32];
const HASH_DOMAIN: &[u8] = b"chat/p0/node-log/v1";
const RECEIPT_DOMAIN: &[u8] = b"chat/p0/node-receipt/v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeReceipt {
    pub group_id: String,
    pub submission_id: String,
    pub seq: u64,
    pub candidate_hash: String,
    pub previous_hash: String,
    pub log_hash: String,
    pub node_public_key: String,
    pub signature: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptCheckpoint {
    AfterLog,
    BeforeCommit,
    AfterCommit,
}

pub struct P0NodeStore {
    connection: Connection,
    signing_key: SigningKey,
}
impl P0NodeStore {
    pub fn open(path: &Path, signing_seed: &[u8; 32]) -> Result<Self> {
        let signing_key = SigningKey::from_bytes(signing_seed);
        let public_key = signing_key.verifying_key().to_bytes();
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS p0_node_identity (
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                public_key BLOB NOT NULL CHECK(length(public_key)=32));
             CREATE TABLE IF NOT EXISTS p0_groups (
                group_id TEXT PRIMARY KEY, last_seq INTEGER NOT NULL CHECK(last_seq>=0),
                last_hash BLOB NOT NULL CHECK(length(last_hash)=32));
             CREATE TABLE IF NOT EXISTS p0_log (
                group_id TEXT NOT NULL REFERENCES p0_groups(group_id),
                seq INTEGER NOT NULL CHECK(seq>0), submission_id TEXT NOT NULL,
                candidate_hash BLOB NOT NULL CHECK(length(candidate_hash)=32),
                ciphertext BLOB NOT NULL, previous_hash BLOB NOT NULL CHECK(length(previous_hash)=32),
                log_hash BLOB NOT NULL CHECK(length(log_hash)=32), receipt BLOB NOT NULL,
                PRIMARY KEY(group_id, seq), UNIQUE(group_id, submission_id));
             CREATE TABLE IF NOT EXISTS p0_delivery_jobs (
                group_id TEXT NOT NULL, seq INTEGER NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('pending','complete')),
                PRIMARY KEY(group_id, seq), FOREIGN KEY(group_id,seq) REFERENCES p0_log(group_id,seq));"
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO p0_node_identity VALUES (1, ?1)",
            [public_key.as_slice()],
        )?;
        let pinned_key: Vec<u8> = connection.query_row(
            "SELECT public_key FROM p0_node_identity WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            pinned_key.as_slice() == public_key.as_slice(),
            "P0 node signing key differs from stored node identity"
        );
        Ok(Self {
            connection,
            signing_key,
        })
    }

    pub fn node_public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub fn init_group(&mut self, group_id: &str) -> Result<()> {
        validate_id(group_id)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO p0_groups VALUES (?1, 0, ?2)",
            params![group_id, ZERO_HASH.as_slice()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn accept(
        &mut self,
        group_id: &str,
        submission_id: &str,
        ciphertext: &[u8],
    ) -> Result<NodeReceipt> {
        self.accept_with_checkpoint(group_id, submission_id, ciphertext, |_| {})
    }

    pub fn accept_with_checkpoint(
        &mut self,
        group_id: &str,
        submission_id: &str,
        ciphertext: &[u8],
        checkpoint: impl Fn(AcceptCheckpoint),
    ) -> Result<NodeReceipt> {
        validate_id(group_id)?;
        validate_id(submission_id)?;
        ensure!(
            !ciphertext.is_empty() && ciphertext.len() <= chat_protocol::wire::MAX_FRAME_BYTES,
            "invalid ciphertext size"
        );
        let candidate_hash = Sha256::digest(ciphertext);
        let node_public_key = self.signing_key.verifying_key().to_bytes();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some((saved_cipher, saved_hash, receipt_bytes, saved_seq, saved_prev, saved_log_hash)) = tx
            .query_row(
                "SELECT ciphertext, candidate_hash, receipt, seq, previous_hash, log_hash FROM p0_log
             WHERE group_id=?1 AND submission_id=?2",
                params![group_id, submission_id],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                        r.get::<_, Vec<u8>>(5)?,
                    ))
                },
            )
            .optional()?
        {
            ensure!(
                saved_cipher == ciphertext && saved_hash == candidate_hash.as_slice(),
                "submission conflict: ciphertext changed"
            );
            let receipt: NodeReceipt = serde_json::from_slice(&receipt_bytes)?;
            ensure!(
                receipt.group_id == group_id
                    && receipt.submission_id == submission_id
                    && receipt.candidate_hash == hex(&candidate_hash)
                    && receipt.seq == saved_seq as u64
                    && receipt.previous_hash == hex(&saved_prev)
                    && receipt.log_hash == hex(&saved_log_hash),
                "stored receipt mismatch"
            );
            verify_receipt(
                &node_public_key,
                group_id,
                submission_id,
                ciphertext,
                &receipt,
            )?;
            let jobs: i64 = tx.query_row(
                "SELECT COUNT(*) FROM p0_delivery_jobs WHERE group_id=?1 AND seq=?2",
                params![group_id, saved_seq], |r| r.get(0))?;
            ensure!(jobs == 1, "accepted log missing delivery job");
            return Ok(receipt);
        }
        let (last_seq, previous_hash): (i64, Vec<u8>) = tx
            .query_row(
                "SELECT last_seq, last_hash FROM p0_groups WHERE group_id=?1",
                [group_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .context("unknown P0 group")?;
        ensure!(previous_hash.len() == 32, "invalid group head hash");
        let seq = last_seq.checked_add(1).context("group sequence overflow")?;
        let log_hash = chain_hash(
            group_id,
            submission_id,
            seq as u64,
            &previous_hash,
            &candidate_hash,
        );
        let mut receipt = NodeReceipt {
            group_id: group_id.to_owned(),
            submission_id: submission_id.to_owned(),
            seq: seq as u64,
            candidate_hash: hex(&candidate_hash),
            previous_hash: hex(&previous_hash),
            log_hash: hex(&log_hash),
            node_public_key: hex(&node_public_key),
            signature: String::new(),
        };
        let signed_bytes = receipt_signing_bytes(
            group_id,
            submission_id,
            seq as u64,
            &candidate_hash,
            &previous_hash,
            &log_hash,
        );
        let signature: Signature = self.signing_key.sign(&signed_bytes);
        receipt.signature = hex(&signature.to_bytes());
        let receipt_bytes = serde_json::to_vec(&receipt)?;
        tx.execute(
            "INSERT INTO p0_log VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                group_id,
                seq,
                submission_id,
                candidate_hash.as_slice(),
                ciphertext,
                &previous_hash,
                log_hash.as_slice(),
                &receipt_bytes
            ],
        )?;
        checkpoint(AcceptCheckpoint::AfterLog);
        tx.execute(
            "INSERT INTO p0_delivery_jobs VALUES (?1,?2,'pending')",
            params![group_id, seq],
        )?;
        tx.execute(
            "UPDATE p0_groups SET last_seq=?2,last_hash=?3 WHERE group_id=?1",
            params![group_id, seq, log_hash.as_slice()],
        )?;
        checkpoint(AcceptCheckpoint::BeforeCommit);
        tx.commit()?;
        checkpoint(AcceptCheckpoint::AfterCommit);
        Ok(receipt)
    }

    pub fn receipt(&self, group_id: &str, submission_id: &str) -> Result<Option<NodeReceipt>> {
        let row: Option<(Vec<u8>, Vec<u8>, i64, Vec<u8>, Vec<u8>, Vec<u8>)> = self
            .connection
            .query_row(
                "SELECT receipt,ciphertext,seq,candidate_hash,previous_hash,log_hash
                 FROM p0_log WHERE group_id=?1 AND submission_id=?2",
                params![group_id, submission_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(bytes, ciphertext, seq, candidate, previous, log_hash)| {
            let receipt: NodeReceipt = serde_json::from_slice(&bytes)?;
            ensure!(
                receipt.seq == seq as u64
                    && receipt.candidate_hash == hex(&candidate)
                    && receipt.previous_hash == hex(&previous)
                    && receipt.log_hash == hex(&log_hash),
                "stored receipt does not match node log"
            );
            verify_receipt(
                &self.node_public_key(),
                group_id,
                submission_id,
                &ciphertext,
                &receipt,
            )?;
            Ok(receipt)
        })
        .transpose()
    }

    pub fn audit(&self, group_id: &str) -> Result<u64> {
        let (head_seq, head_hash): (i64, Vec<u8>) = self.connection.query_row(
            "SELECT last_seq,last_hash FROM p0_groups WHERE group_id=?1",
            [group_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT seq,submission_id,candidate_hash,ciphertext,previous_hash,log_hash,receipt
             FROM p0_log WHERE group_id=?1 ORDER BY seq",
        )?;
        let rows = statement.query_map([group_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, Vec<u8>>(4)?,
                r.get::<_, Vec<u8>>(5)?,
                r.get::<_, Vec<u8>>(6)?,
            ))
        })?;
        let mut seq = 0_i64;
        let mut prev = ZERO_HASH.to_vec();
        for row in rows {
            let (actual_seq, id, candidate, cipher, actual_prev, actual_hash, receipt_bytes) = row?;
            seq += 1;
            ensure!(
                actual_seq == seq && actual_prev == prev,
                "non-contiguous node log"
            );
            ensure!(
                Sha256::digest(&cipher).as_slice() == candidate,
                "candidate hash mismatch"
            );
            let expected = chain_hash(group_id, &id, seq as u64, &prev, &candidate);
            ensure!(actual_hash == expected, "node hash-chain mismatch");
            let receipt: NodeReceipt = serde_json::from_slice(&receipt_bytes)?;
            ensure!(
                receipt
                    == NodeReceipt {
                        group_id: group_id.to_owned(),
                        submission_id: id,
                        seq: seq as u64,
                        candidate_hash: hex(&candidate),
                        previous_hash: hex(&prev),
                        log_hash: hex(&expected),
                        node_public_key: hex(&self.node_public_key()),
                        signature: receipt.signature.clone()
                    },
                "node receipt mismatch"
            );
            verify_receipt(
                &self.node_public_key(),
                group_id,
                &receipt.submission_id,
                &cipher,
                &receipt,
            )?;
            let jobs: i64 = self.connection.query_row(
                "SELECT COUNT(*) FROM p0_delivery_jobs WHERE group_id=?1 AND seq=?2",
                params![group_id, seq],
                |r| r.get(0),
            )?;
            ensure!(jobs == 1, "accepted log missing delivery job");
            prev = expected.to_vec();
        }
        ensure!(seq == head_seq && prev == head_hash, "node head mismatch");
        Ok(seq as u64)
    }
}

/// Verify against a separately pinned node public key, never a key supplied
/// only inside the receipt. This is a P0 encoding, not the final wire format.
pub fn verify_receipt(
    expected_node_key: &[u8; 32],
    group_id: &str,
    submission_id: &str,
    ciphertext: &[u8],
    receipt: &NodeReceipt,
) -> Result<()> {
    validate_id(group_id)?;
    validate_id(submission_id)?;
    ensure!(
        receipt.group_id == group_id && receipt.submission_id == submission_id,
        "receipt scope mismatch"
    );
    ensure!(receipt.seq > 0, "invalid receipt sequence");
    ensure!(
        receipt.node_public_key == hex(expected_node_key),
        "receipt signer does not match pinned node key"
    );
    let candidate = Sha256::digest(ciphertext);
    ensure!(
        receipt.candidate_hash == hex(&candidate),
        "receipt ciphertext digest mismatch"
    );
    let previous = decode_hex::<32>(&receipt.previous_hash)?;
    let log_hash = chain_hash(group_id, submission_id, receipt.seq, &previous, &candidate);
    ensure!(
        receipt.log_hash == hex(&log_hash),
        "receipt log hash mismatch"
    );
    let signature = Signature::from_bytes(&decode_hex::<64>(&receipt.signature)?);
    let key = VerifyingKey::from_bytes(expected_node_key)?;
    key.verify_strict(
        &receipt_signing_bytes(
            group_id,
            submission_id,
            receipt.seq,
            &candidate,
            &previous,
            &log_hash,
        ),
        &signature,
    )?;
    Ok(())
}

fn receipt_signing_bytes(
    group: &str,
    submission: &str,
    seq: u64,
    candidate: &[u8],
    previous: &[u8],
    log_hash: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);
    bytes.extend_from_slice(RECEIPT_DOMAIN);
    for value in [group.as_bytes(), submission.as_bytes()] {
        bytes.extend_from_slice(&(value.len() as u16).to_be_bytes());
        bytes.extend_from_slice(value);
    }
    bytes.extend_from_slice(&seq.to_be_bytes());
    bytes.extend_from_slice(candidate);
    bytes.extend_from_slice(previous);
    bytes.extend_from_slice(log_hash);
    bytes
}

fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N]> {
    let bytes = value.as_bytes();
    ensure!(bytes.len() == N * 2, "invalid hex length");
    let mut result = [0_u8; N];
    for (index, slot) in result.iter_mut().enumerate() {
        let pair = std::str::from_utf8(&bytes[index * 2..index * 2 + 2])?;
        *slot = u8::from_str_radix(pair, 16)?;
    }
    Ok(result)
}

fn chain_hash(group: &str, submission: &str, seq: u64, prev: &[u8], candidate: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(HASH_DOMAIN);
    for value in [group.as_bytes(), submission.as_bytes()] {
        hasher.update((value.len() as u16).to_be_bytes());
        hasher.update(value);
    }
    hasher.update(seq.to_be_bytes());
    hasher.update(prev);
    hasher.update(candidate);
    hasher.finalize().into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid P0 node ID"
    );
    Ok(())
}
