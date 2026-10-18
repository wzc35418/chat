//! Ephemeral P0 vertical slice. It does not bypass or implement MVP coordinator semantics.
use anyhow::{Context, Result, ensure};
use chat_crypto::engine::{MlsDevice, SUITE_NAME};
use chat_protocol::wire::{FrameKind, decode_frame, encode_frame, validate_text};
use chat_transport::network::LocalTransport;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::runtime::{Builder, Runtime};

#[derive(Clone, Copy, Debug)]
pub enum Side {
    Alice,
    Bob,
}
impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Self::Alice => "Alice",
            Self::Bob => "Bob",
        }
    }
    pub fn peer(self) -> &'static str {
        match self {
            Self::Alice => "Bob",
            Self::Bob => "Alice",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionInfo {
    pub alice_endpoint: String,
    pub bob_endpoint: String,
    pub group_id: String,
    pub epoch: u64,
    pub ciphersuite: String,
    pub transport: String,
    pub public_infrastructure: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Delivery {
    pub sender: String,
    pub receiver: String,
    pub text: String,
    pub ciphertext_bytes: u32,
    pub ciphertext_sha256: String,
    pub epoch: u64,
    pub exchange_index: u64,
    pub elapsed_ms: u64,
}

struct Pair {
    alice: MlsDevice,
    bob: MlsDevice,
    network: LocalTransport,
    info: SessionInfo,
    exchange_index: u64,
    failed: bool,
}
impl Pair {
    async fn new() -> Result<Self> {
        let network = LocalTransport::new().await?;
        let result = async {
            let mut alice = MlsDevice::new("Alice")?;
            let mut bob = MlsDevice::new("Bob")?;
            let key_package = encode_frame(FrameKind::KeyPackage, &bob.key_package()?)?;
            let received = network.transfer(false, &key_package).await?;
            let welcome =
                alice.create_and_invite(decode_frame(&received, FrameKind::KeyPackage)?, &bob)?;
            let received = network
                .transfer(true, &encode_frame(FrameKind::Welcome, &welcome)?)
                .await?;
            bob.join(decode_frame(&received, FrameKind::Welcome)?, &alice)?;
            let info = SessionInfo {
                alice_endpoint: network.alice.id().to_string(),
                bob_endpoint: network.bob.id().to_string(),
                group_id: alice.group_id()?,
                epoch: alice.epoch()?,
                ciphersuite: SUITE_NAME.into(),
                transport: "iroh / QUIC / 127.0.0.1".into(),
                public_infrastructure: false,
            };
            Ok::<_, anyhow::Error>((alice, bob, info))
        }
        .await;
        match result {
            Ok((alice, bob, info)) => Ok(Self {
                alice,
                bob,
                network,
                info,
                exchange_index: 0,
                failed: false,
            }),
            Err(error) => {
                network.close().await;
                Err(error)
            }
        }
    }
    async fn send(&mut self, side: Side, text: &str) -> Result<Delivery> {
        let started = Instant::now();
        let from_alice = matches!(side, Side::Alice);
        let ciphertext = if from_alice {
            self.alice.encrypt(text)?
        } else {
            self.bob.encrypt(text)?
        };
        let received = self
            .network
            .transfer(
                from_alice,
                &encode_frame(FrameKind::Application, &ciphertext)?,
            )
            .await?;
        let payload = decode_frame(&received, FrameKind::Application)?;
        ensure!(payload == ciphertext, "ciphertext transport mismatch");
        let plaintext = if from_alice {
            self.bob.decrypt(payload, side.name())?
        } else {
            self.alice.decrypt(payload, side.name())?
        };
        ensure!(plaintext == text, "MLS roundtrip mismatch");
        self.exchange_index += 1;
        Ok(Delivery {
            sender: side.name().into(),
            receiver: side.peer().into(),
            text: plaintext,
            ciphertext_bytes: ciphertext.len() as u32,
            ciphertext_sha256: Sha256::digest(&ciphertext)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            epoch: self.alice.epoch()?,
            exchange_index: self.exchange_index,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }
}

pub struct P0Session {
    state: Mutex<Option<Pair>>,
    runtime: Runtime,
}
impl P0Session {
    pub fn new() -> Result<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("chat-p0")
            .build()?;
        let pair = runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(30), Pair::new()).await })
            .context("P0 initialization timed out")??;
        Ok(Self {
            state: Mutex::new(Some(pair)),
            runtime,
        })
    }
    pub fn info(&self) -> Result<SessionInfo> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("P0 session state poisoned"))?;
        Ok(state.as_ref().context("P0 session closed")?.info.clone())
    }
    pub fn send(&self, side: Side, text: &str) -> Result<Delivery> {
        validate_text(text)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("P0 session state poisoned"))?;
        let pair = state.as_mut().context("P0 session closed")?;
        ensure!(!pair.failed, "P0 session failed; create a fresh session");
        let result = self.runtime.block_on(pair.send(side, text));
        // No durability/reconciliation yet: never reuse a partially consumed MLS state after failure.
        if result.is_err() {
            pair.failed = true;
        }
        result
    }
    pub fn close(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("P0 session state poisoned"))?;
        if let Some(pair) = state.take() {
            self.runtime
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), pair.network.close()).await
                })
                .context("P0 shutdown timed out")?;
        }
        Ok(())
    }
}
impl Drop for P0Session {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
