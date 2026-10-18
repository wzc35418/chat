use chat_core::p0::{P0Session, Side};
use std::sync::Arc;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum P0Error {
    #[error("{reason}")]
    InvalidInput { reason: String },
    #[error("P0 operation failed: {reason}")]
    OperationFailed { reason: String },
}
fn failure(error: impl std::fmt::Display) -> P0Error {
    P0Error::OperationFailed {
        reason: error.to_string(),
    }
}

#[derive(uniffi::Record)]
pub struct P0Info {
    pub alice_endpoint: String,
    pub bob_endpoint: String,
    pub group_id: String,
    pub epoch: u64,
    pub ciphersuite: String,
    pub transport: String,
    pub public_infrastructure: bool,
}
#[derive(uniffi::Record)]
pub struct P0Message {
    pub sender: String,
    pub receiver: String,
    pub text: String,
    pub ciphertext_bytes: u32,
    pub ciphertext_sha256: String,
    pub epoch: u64,
    pub exchange_index: u64,
    pub elapsed_ms: u64,
}
#[derive(uniffi::Object)]
pub struct P0Core {
    session: P0Session,
}

#[uniffi::export]
impl P0Core {
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<Self>, P0Error> {
        Ok(Arc::new(Self {
            session: P0Session::new().map_err(failure)?,
        }))
    }
    pub fn info(&self) -> Result<P0Info, P0Error> {
        let info = self.session.info().map_err(failure)?;
        Ok(P0Info {
            alice_endpoint: info.alice_endpoint,
            bob_endpoint: info.bob_endpoint,
            group_id: info.group_id,
            epoch: info.epoch,
            ciphersuite: info.ciphersuite,
            transport: info.transport,
            public_infrastructure: info.public_infrastructure,
        })
    }
    pub fn send(&self, sender: String, text: String) -> Result<P0Message, P0Error> {
        chat_protocol::wire::validate_text(&text).map_err(|e| P0Error::InvalidInput {
            reason: e.to_string(),
        })?;
        let side = match sender.as_str() {
            "Alice" => Side::Alice,
            "Bob" => Side::Bob,
            _ => {
                return Err(P0Error::InvalidInput {
                    reason: "unknown sender".into(),
                });
            }
        };
        let message = self.session.send(side, &text).map_err(failure)?;
        Ok(P0Message {
            sender: message.sender,
            receiver: message.receiver,
            text: message.text,
            ciphertext_bytes: message.ciphertext_bytes,
            ciphertext_sha256: message.ciphertext_sha256,
            epoch: message.epoch,
            exchange_index: message.exchange_index,
            elapsed_ms: message.elapsed_ms,
        })
    }
    pub fn shutdown(&self) -> Result<(), P0Error> {
        self.session.close().map_err(failure)
    }
}
