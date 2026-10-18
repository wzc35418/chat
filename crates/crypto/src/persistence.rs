//! P0-only crash experiment. Secrets in these disposable SQLite files are unencrypted.
//! Every MLS operation creates a fresh group from the DB, uses the same SQLite
//! transaction as the attempt/outbox, then drops it before commit or rollback.
use anyhow::{Context, Result, bail, ensure};
use chat_storage::store::{JsonCodec, P0Intent, P0Store};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::RustCrypto;
use openmls_sqlite_storage::SqliteStorageProvider;
use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::path::Path;

use crate::engine::SUITE;

struct Provider<'a> {
    crypto: RustCrypto,
    storage: SqliteStorageProvider<JsonCodec, &'a rusqlite::Connection>,
}
impl<'a> Provider<'a> {
    fn new(tx: &'a Transaction<'_>) -> Self {
        Self {
            crypto: RustCrypto::default(),
            storage: SqliteStorageProvider::new(&**tx),
        }
    }
}
impl<'a> OpenMlsProvider for Provider<'a> {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = SqliteStorageProvider<JsonCodec, &'a rusqlite::Connection>;
    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }
    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
    fn storage(&self) -> &Self::StorageProvider {
        &self.storage
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checkpoint {
    AfterMls,
    AfterAttempt,
    AfterCommit,
}

pub struct DurableDevice {
    store: P0Store,
    name: String,
    signer: SignatureKeyPair,
    credential: CredentialWithKey,
    group_id: Option<Vec<u8>>,
}
impl DurableDevice {
    pub fn create(path: &Path, name: &str) -> Result<Self> {
        let mut store = P0Store::open(path)?;
        let tx = store.begin()?;
        ensure!(
            tx.query_row("SELECT COUNT(*) FROM p0_meta", [], |r| r.get::<_, i64>(0))? == 0,
            "device already exists"
        );
        let signer = SignatureKeyPair::new(SUITE.signature_algorithm())?;
        {
            let provider = Provider::new(&tx);
            signer.store(provider.storage())?;
        }
        tx.execute("INSERT INTO p0_meta VALUES ('name', ?1)", [name.as_bytes()])?;
        tx.execute(
            "INSERT INTO p0_meta VALUES ('signing_public', ?1)",
            [signer.public()],
        )?;
        tx.commit()?;
        Self::open(path)
    }
    pub fn open(path: &Path) -> Result<Self> {
        let store = P0Store::open(path)?;
        let name_bytes: Vec<u8> =
            store
                .connection
                .query_row("SELECT value FROM p0_meta WHERE key='name'", [], |r| {
                    r.get(0)
                })?;
        let name = String::from_utf8(name_bytes)?;
        let public: Vec<u8> = store.connection.query_row(
            "SELECT value FROM p0_meta WHERE key='signing_public'",
            [],
            |r| r.get(0),
        )?;
        let provider =
            SqliteStorageProvider::<JsonCodec, &rusqlite::Connection>::new(&store.connection);
        let signer = SignatureKeyPair::read(&provider, &public, SUITE.signature_algorithm())
            .context("missing persisted signer")?;
        let credential = CredentialWithKey {
            credential: BasicCredential::new(name.as_bytes().to_vec()).into(),
            signature_key: public.into(),
        };
        let group_id = store
            .connection
            .query_row("SELECT value FROM p0_meta WHERE key='group_id'", [], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(Self {
            store,
            name,
            signer,
            credential,
            group_id,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn public_key(&self) -> &[u8] {
        self.credential.signature_key.as_slice()
    }
    pub fn group_id(&self) -> Option<&[u8]> {
        self.group_id.as_deref()
    }
    pub fn key_package(&mut self) -> Result<Vec<u8>> {
        let tx = self.store.begin()?;
        let bytes = {
            let provider = Provider::new(&tx);
            KeyPackage::builder()
                .build(SUITE, &provider, &self.signer, self.credential.clone())?
                .key_package()
                .tls_serialize_detached()?
        };
        tx.commit()?;
        Ok(bytes)
    }
    pub fn invite(
        &mut self,
        package: &[u8],
        peer_name: &str,
        peer_public: &[u8],
    ) -> Result<Vec<u8>> {
        ensure!(self.group_id.is_none(), "group already initialized");
        let tx = self.store.begin()?;
        let (welcome, id) = {
            let provider = Provider::new(&tx);
            let package = KeyPackageIn::tls_deserialize_exact(package)?
                .validate(provider.crypto(), ProtocolVersion::Mls10)?;
            ensure!(
                package.leaf_node().credential().serialized_content() == peer_name.as_bytes(),
                "peer identity mismatch"
            );
            ensure!(
                package.leaf_node().signature_key().as_slice() == peer_public,
                "peer signing key mismatch"
            );
            let config = MlsGroupCreateConfig::builder()
                .ciphersuite(SUITE)
                .use_ratchet_tree_extension(true)
                .build();
            let mut group = MlsGroup::new_with_group_id(
                &provider,
                &self.signer,
                &config,
                GroupId::from_slice(uuid::Uuid::new_v4().as_bytes()),
                self.credential.clone(),
            )?;
            let (_, welcome, _) = group.add_members(&provider, &self.signer, &[package])?;
            group.merge_pending_commit(&provider)?;
            (
                welcome.tls_serialize_detached()?,
                group.group_id().as_slice().to_vec(),
            )
        };
        tx.execute("INSERT INTO p0_meta VALUES ('group_id', ?1)", [&id])?;
        tx.commit()?;
        self.group_id = Some(id);
        Ok(welcome)
    }
    pub fn join(&mut self, welcome: &[u8], expected_id: &[u8]) -> Result<()> {
        ensure!(self.group_id.is_none(), "group already initialized");
        let tx = self.store.begin()?;
        let id = {
            let provider = Provider::new(&tx);
            let message = MlsMessageIn::tls_deserialize_exact(welcome)?;
            let MlsMessageBodyIn::Welcome(welcome) = message.extract() else {
                bail!("expected Welcome")
            };
            let config = MlsGroupJoinConfig::builder()
                .use_ratchet_tree_extension(true)
                .build();
            let group = StagedWelcome::new_from_welcome(&provider, &config, welcome, None)?
                .into_group(&provider)?;
            ensure!(group.group_id().as_slice() == expected_id, "group mismatch");
            group.group_id().as_slice().to_vec()
        };
        tx.execute("INSERT INTO p0_meta VALUES ('group_id', ?1)", [&id])?;
        tx.commit()?;
        self.group_id = Some(id);
        Ok(())
    }
    pub fn prepare(&mut self, submission: &str, text: &str) -> Result<Vec<u8>> {
        self.prepare_with_checkpoint(submission, text, |_| {})
    }
    pub fn persist_intent(&mut self, draft_id: &str, operation_id: &str, text: &str) -> Result<()> {
        self.store.persist_intent(draft_id, operation_id, text)
    }
    pub fn intent(&self, draft_id: &str) -> Result<Option<P0Intent>> {
        self.store.intent(draft_id)
    }
    pub fn waiting_intents(&self) -> Result<Vec<String>> {
        self.store.waiting_intents()
    }
    pub fn prepare_intent(&mut self, draft_id: &str, submission: &str) -> Result<Vec<u8>> {
        self.prepare_intent_with_checkpoint(draft_id, submission, |_| {})
    }
    pub fn prepare_intent_with_checkpoint(
        &mut self,
        draft_id: &str,
        submission: &str,
        checkpoint: impl Fn(Checkpoint),
    ) -> Result<Vec<u8>> {
        ensure!(!submission.is_empty(), "empty submission ID");
        let tx = self.store.begin()?;
        let (body, request_digest, state, prior_submission, job_state): (
            Vec<u8>,
            Vec<u8>,
            String,
            Option<String>,
            String,
        ) = tx
            .query_row(
                "SELECT i.body, i.request_digest, i.state, i.submission_id, j.state
             FROM p0_intents i JOIN p0_prepare_jobs j USING(draft_id)
             WHERE i.draft_id=?1",
                [draft_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .context("missing persisted intent or prepare job")?;
        ensure!(
            Sha256::digest(&body).as_slice() == request_digest,
            "intent request digest mismatch"
        );
        if state == "prepared" {
            ensure!(
                job_state == "complete" && prior_submission.as_deref() == Some(submission),
                "intent already prepared with a different submission"
            );
            let (cipher, digest): (Vec<u8>, Vec<u8>) = tx.query_row(
                "SELECT cipher, digest FROM p0_attempts WHERE submission_id=?1",
                [submission],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let outbox_rows: i64 = tx.query_row(
                "SELECT COUNT(*) FROM p0_outbox WHERE submission_id=?1",
                [submission],
                |row| row.get(0),
            )?;
            ensure!(outbox_rows == 1, "prepared intent missing outbox job");
            ensure!(
                Sha256::digest(&cipher).as_slice() == digest,
                "prepared cipher digest mismatch"
            );
            return Ok(cipher);
        }
        ensure!(
            state == "queued" && job_state == "waiting" && prior_submission.is_none(),
            "inconsistent intent state"
        );
        chat_protocol::wire::validate_text(std::str::from_utf8(&body)?)?;
        let cipher = {
            let provider = Provider::new(&tx);
            let id =
                GroupId::from_slice(self.group_id.as_deref().context("group not initialized")?);
            let mut group =
                MlsGroup::load(provider.storage(), &id)?.context("missing persisted group")?;
            group
                .create_message(&provider, &self.signer, &body)?
                .tls_serialize_detached()?
        };
        checkpoint(Checkpoint::AfterMls);
        let digest = Sha256::digest(&cipher);
        tx.execute(
            "INSERT INTO p0_attempts VALUES (?1, ?2, ?3, ?4)",
            params![submission, &cipher, digest.as_slice(), &request_digest],
        )?;
        checkpoint(Checkpoint::AfterAttempt);
        tx.execute("INSERT INTO p0_outbox VALUES (?1, 'pending')", [submission])?;
        tx.execute(
            "UPDATE p0_intents SET state='prepared', submission_id=?2 WHERE draft_id=?1",
            params![draft_id, submission],
        )?;
        tx.execute(
            "UPDATE p0_prepare_jobs SET state='complete' WHERE draft_id=?1",
            [draft_id],
        )?;
        tx.commit()?;
        checkpoint(Checkpoint::AfterCommit);
        Ok(cipher)
    }
    pub fn prepare_with_checkpoint(
        &mut self,
        submission: &str,
        text: &str,
        checkpoint: impl Fn(Checkpoint),
    ) -> Result<Vec<u8>> {
        chat_protocol::wire::validate_text(text)?;
        ensure!(!submission.is_empty(), "empty submission ID");
        let request_digest = Sha256::digest(text.as_bytes());
        let tx = self.store.begin()?;
        if let Some((cipher, old_digest)) = tx
            .query_row(
                "SELECT cipher, request_digest FROM p0_attempts WHERE submission_id=?1",
                [submission],
                |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?)),
            )
            .optional()?
        {
            ensure!(
                old_digest == request_digest.as_slice(),
                "submission ID reused with different text"
            );
            return Ok(cipher);
        }
        let cipher = {
            let provider = Provider::new(&tx);
            let id =
                GroupId::from_slice(self.group_id.as_deref().context("group not initialized")?);
            let mut group =
                MlsGroup::load(provider.storage(), &id)?.context("missing persisted group")?;
            group
                .create_message(&provider, &self.signer, text.as_bytes())?
                .tls_serialize_detached()?
        };
        checkpoint(Checkpoint::AfterMls);
        let digest = Sha256::digest(&cipher);
        tx.execute(
            "INSERT INTO p0_attempts VALUES (?1, ?2, ?3, ?4)",
            params![
                submission,
                &cipher,
                digest.as_slice(),
                request_digest.as_slice()
            ],
        )?;
        checkpoint(Checkpoint::AfterAttempt);
        tx.execute("INSERT INTO p0_outbox VALUES (?1, 'pending')", [submission])?;
        tx.commit()?;
        checkpoint(Checkpoint::AfterCommit);
        Ok(cipher)
    }
    pub fn pending(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let mut statement = self.store.connection.prepare("SELECT a.submission_id, a.cipher, a.digest FROM p0_attempts a JOIN p0_outbox o USING(submission_id) WHERE o.state='pending' ORDER BY a.submission_id")?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (id, cipher, digest) = row?;
            ensure!(
                Sha256::digest(&cipher).as_slice() == digest,
                "outbox cipher digest mismatch"
            );
            result.push((id, cipher));
        }
        Ok(result)
    }
    pub fn receive(&mut self, submission: &str, cipher: &[u8], sender: &str) -> Result<String> {
        self.receive_with_checkpoint(submission, cipher, sender, |_| {})
    }
    pub fn receive_with_checkpoint(
        &mut self,
        submission: &str,
        cipher: &[u8],
        sender: &str,
        checkpoint: impl Fn(Checkpoint),
    ) -> Result<String> {
        ensure!(
            cipher.len() <= chat_protocol::wire::MAX_FRAME_BYTES,
            "ciphertext exceeds limit"
        );
        let digest = Sha256::digest(cipher);
        let tx = self.store.begin()?;
        if let Some((body, old_digest, old_sender)) = tx
            .query_row(
                "SELECT body, digest, sender FROM p0_inbox WHERE submission_id=?1",
                [submission],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?
        {
            ensure!(
                old_digest == digest.as_slice(),
                "submission ID reused with different ciphertext"
            );
            ensure!(
                old_sender == sender,
                "submission ID reused with different sender"
            );
            return Ok(String::from_utf8(body)?);
        }
        let body = {
            let provider = Provider::new(&tx);
            let id =
                GroupId::from_slice(self.group_id.as_deref().context("group not initialized")?);
            let mut group =
                MlsGroup::load(provider.storage(), &id)?.context("missing persisted group")?;
            let message =
                MlsMessageIn::tls_deserialize_exact(cipher)?.try_into_protocol_message()?;
            let processed = group.process_message(&provider, message)?;
            ensure!(
                processed.credential().serialized_content() == sender.as_bytes(),
                "wrong authenticated sender"
            );
            match processed.into_content() {
                ProcessedMessageContent::ApplicationMessage(message) => message.into_bytes(),
                _ => bail!("expected application message"),
            }
        };
        checkpoint(Checkpoint::AfterMls);
        tx.execute(
            "INSERT INTO p0_inbox VALUES (?1, ?2, ?3, ?4)",
            params![submission, &body, digest.as_slice(), sender],
        )?;
        checkpoint(Checkpoint::AfterAttempt);
        tx.commit()?;
        checkpoint(Checkpoint::AfterCommit);
        Ok(String::from_utf8(body)?)
    }
}
