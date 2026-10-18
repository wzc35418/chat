//! P0 real QUIC loopback transport, without public discovery or relays.
use anyhow::{Context, Result, ensure};
use chat_protocol::wire::{MAX_FRAME_BYTES, P0_ALPN};
use iroh::{
    Endpoint, EndpointAddr, RelayMode,
    endpoint::{NetReportConfig, PortmapperConfig, presets},
};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub struct LocalTransport {
    pub alice: Endpoint,
    pub bob: Endpoint,
}

impl LocalTransport {
    pub async fn new() -> Result<Self> {
        let alice = Self::endpoint().await?;
        let bob = match Self::endpoint().await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                alice.close().await;
                return Err(error);
            }
        };
        Ok(Self { alice, bob })
    }
    async fn endpoint() -> Result<Endpoint> {
        Ok(Endpoint::builder(presets::Minimal)
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")?
            .relay_mode(RelayMode::Disabled)
            .portmapper_config(PortmapperConfig::Disabled)
            .net_report_config(NetReportConfig::minimal())
            .clear_address_lookup()
            .alpns(vec![P0_ALPN.to_vec()])
            .bind()
            .await?)
    }
    pub fn address(endpoint: &Endpoint) -> Result<EndpointAddr> {
        let sockets = endpoint.bound_sockets();
        ensure!(
            sockets.len() == 1 && sockets[0].ip().is_loopback(),
            "expected loopback-only socket"
        );
        Ok(EndpointAddr::new(endpoint.id()).with_ip_addr(sockets[0]))
    }
    pub async fn transfer(&self, from_alice: bool, frame: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            !frame.is_empty() && frame.len() <= MAX_FRAME_BYTES,
            "invalid transport frame size"
        );
        let (sender, receiver) = if from_alice {
            (&self.alice, &self.bob)
        } else {
            (&self.bob, &self.alice)
        };
        let target = Self::address(receiver)?;
        let expected_id = sender.id();
        let client = async {
            let connection = sender.connect(target, P0_ALPN).await?;
            ensure!(
                connection.remote_id() == receiver.id(),
                "wrong receiving endpoint"
            );
            let (mut send, mut recv) = connection.open_bi().await?;
            send.write_all(frame).await?;
            send.finish()?;
            let ack = recv.read_to_end(32).await?;
            ensure!(
                ack.as_slice() == Sha256::digest(frame).as_slice(),
                "invalid transport receipt"
            );
            connection.close(0u32.into(), b"p0 exchange complete");
            Ok::<_, anyhow::Error>(())
        };
        let server = async {
            let incoming = receiver.accept().await.context("endpoint closed")?;
            let connection = incoming.await?;
            ensure!(
                connection.remote_id() == expected_id,
                "unexpected sending endpoint"
            );
            let (mut send, mut recv) = connection.accept_bi().await?;
            let frame = recv.read_to_end(MAX_FRAME_BYTES).await?;
            ensure!(!frame.is_empty(), "empty transport frame");
            send.write_all(&Sha256::digest(&frame)).await?;
            send.finish()?;
            connection.closed().await;
            Ok::<_, anyhow::Error>(frame)
        };
        let (_, received) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::try_join!(client, server)
        })
        .await
        .context("P0 transport timed out")??;
        Ok(received)
    }
    pub async fn close(&self) {
        tokio::join!(self.alice.close(), self.bob.close());
    }
}
