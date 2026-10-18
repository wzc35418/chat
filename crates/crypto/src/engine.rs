//! Real MLS for the ephemeral P0 pair; persistence is deliberately not implemented here.
use anyhow::{Context, Result, bail, ensure};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

pub const SUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
pub const SUITE_NAME: &str = "MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519";

pub struct MlsDevice {
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    credential: CredentialWithKey,
    group: Option<MlsGroup>,
}

impl MlsDevice {
    pub fn new(name: &str) -> Result<Self> {
        let provider = OpenMlsRustCrypto::default();
        let signer = SignatureKeyPair::new(SUITE.signature_algorithm())
            .context("create device signing key")?;
        signer
            .store(provider.storage())
            .context("store ephemeral signing key")?;
        let credential = CredentialWithKey {
            credential: BasicCredential::new(name.as_bytes().to_vec()).into(),
            signature_key: signer.public().into(),
        };
        Ok(Self {
            provider,
            signer,
            credential,
            group: None,
        })
    }

    pub fn key_package(&self) -> Result<Vec<u8>> {
        let bundle = KeyPackage::builder()
            .build(SUITE, &self.provider, &self.signer, self.credential.clone())
            .context("create MLS KeyPackage")?;
        Ok(bundle.key_package().tls_serialize_detached()?)
    }

    pub fn create_and_invite(&mut self, key_package: &[u8], peer: &MlsDevice) -> Result<Vec<u8>> {
        ensure!(self.group.is_none(), "MLS group already initialized");
        let key_package = KeyPackageIn::tls_deserialize_exact(key_package)?
            .validate(self.provider.crypto(), ProtocolVersion::Mls10)
            .context("validate MLS KeyPackage")?;
        ensure!(
            key_package.leaf_node().credential() == &peer.credential.credential,
            "untrusted peer credential"
        );
        ensure!(
            key_package.leaf_node().signature_key() == &peer.credential.signature_key,
            "untrusted peer signing key"
        );
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(SUITE)
            .use_ratchet_tree_extension(true)
            .build();
        let mut group = MlsGroup::new_with_group_id(
            &self.provider,
            &self.signer,
            &config,
            GroupId::from_slice(uuid::Uuid::new_v4().as_bytes()),
            self.credential.clone(),
        )
        .context("create MLS group")?;
        let (_, welcome, _) = group
            .add_members(&self.provider, &self.signer, &[key_package])
            .context("stage MLS member addition")?;
        // P0 has no coordinator: accept the local bootstrap commit, not a product group operation.
        group
            .merge_pending_commit(&self.provider)
            .context("merge bootstrap pending commit")?;
        let bytes = welcome.tls_serialize_detached()?;
        self.group = Some(group);
        Ok(bytes)
    }

    pub fn join(&mut self, welcome: &[u8], creator: &MlsDevice) -> Result<()> {
        ensure!(self.group.is_none(), "MLS group already initialized");
        let message = MlsMessageIn::tls_deserialize_exact(welcome)?;
        let MlsMessageBodyIn::Welcome(welcome) = message.extract() else {
            bail!("expected MLS Welcome");
        };
        let config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        let group = StagedWelcome::new_from_welcome(&self.provider, &config, welcome, None)
            .context("stage MLS Welcome")?
            .into_group(&self.provider)
            .context("join MLS group")?;
        let members = group.members().collect::<Vec<_>>();
        ensure!(members.len() == 2, "unexpected bootstrap membership");
        for expected in [&self.credential, &creator.credential] {
            ensure!(
                members.iter().any(|m| m.credential == expected.credential
                    && m.signature_key == expected.signature_key.as_slice()),
                "bootstrap identity binding mismatch"
            );
        }
        ensure!(
            group.group_id().as_slice() == creator.group()?.group_id().as_slice(),
            "wrong MLS group"
        );
        ensure!(
            group.epoch_authenticator().as_slice()
                == creator.group()?.epoch_authenticator().as_slice(),
            "MLS authenticator mismatch"
        );
        self.group = Some(group);
        Ok(())
    }

    fn group(&self) -> Result<&MlsGroup> {
        self.group.as_ref().context("MLS group is not initialized")
    }
    pub fn epoch(&self) -> Result<u64> {
        Ok(self.group()?.epoch().as_u64())
    }
    pub fn group_id(&self) -> Result<String> {
        Ok(self
            .group()?
            .group_id()
            .as_slice()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }

    pub fn encrypt(&mut self, text: &str) -> Result<Vec<u8>> {
        chat_protocol::wire::validate_text(text)?;
        let group = self
            .group
            .as_mut()
            .context("MLS group is not initialized")?;
        Ok(group
            .create_message(&self.provider, &self.signer, text.as_bytes())
            .context("encrypt MLS application message")?
            .tls_serialize_detached()?)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8], expected_sender: &str) -> Result<String> {
        ensure!(
            ciphertext.len() <= chat_protocol::wire::MAX_FRAME_BYTES,
            "ciphertext exceeds limit"
        );
        let message = MlsMessageIn::tls_deserialize_exact(ciphertext)?
            .try_into_protocol_message()
            .context("expected MLS application protocol message")?;
        let group = self
            .group
            .as_mut()
            .context("MLS group is not initialized")?;
        let processed = group
            .process_message(&self.provider, message)
            .context("authenticate/decrypt MLS message")?;
        ensure!(
            processed.credential().serialized_content() == expected_sender.as_bytes(),
            "unexpected authenticated sender"
        );
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(message) => {
                Ok(String::from_utf8(message.into_bytes())?)
            }
            _ => bail!("expected application content"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (MlsDevice, MlsDevice) {
        let mut alice = MlsDevice::new("Alice").unwrap();
        let mut bob = MlsDevice::new("Bob").unwrap();
        let welcome = alice
            .create_and_invite(&bob.key_package().unwrap(), &bob)
            .unwrap();
        bob.join(&welcome, &alice).unwrap();
        (alice, bob)
    }
    #[test]
    fn actual_mls_bidirectional_and_replay_rejection() {
        let (mut alice, mut bob) = pair();
        let ciphertext = alice.encrypt("你好，MLS 👋").unwrap();
        assert!(
            !ciphertext
                .windows("你好，MLS 👋".len())
                .any(|w| w == "你好，MLS 👋".as_bytes())
        );
        assert_eq!(bob.decrypt(&ciphertext, "Alice").unwrap(), "你好，MLS 👋");
        assert!(bob.decrypt(&ciphertext, "Alice").is_err());
        assert_eq!(
            alice.decrypt(&bob.encrypt("收到").unwrap(), "Bob").unwrap(),
            "收到"
        );
    }
    #[test]
    fn tampered_foreign_group_and_wrong_identity_rejected() {
        let (mut alice, mut bob) = pair();
        let (_, mut outsider) = pair();
        let ciphertext = alice.encrypt("confidential P0 test message").unwrap();
        assert!(outsider.decrypt(&ciphertext, "Alice").is_err());
        let mut tampered = ciphertext.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(bob.decrypt(&tampered, "Alice").is_err());
        // The provider may consume the receive secret before authentication fails.
        // P0 must fail the session rather than assuming invalid ciphertext is side-effect free.
        assert!(bob.decrypt(&ciphertext, "Alice").is_err());
        let (mut alice, mut bob) = pair();
        let ciphertext = alice.encrypt("identity check").unwrap();
        assert!(bob.decrypt(&ciphertext, "Mallory").is_err());
    }
    #[test]
    fn key_package_binding_and_single_use_welcome() {
        let mut alice = MlsDevice::new("Alice").unwrap();
        let mut bob = MlsDevice::new("Bob").unwrap();
        let mallory = MlsDevice::new("Mallory").unwrap();
        assert!(
            alice
                .create_and_invite(&mallory.key_package().unwrap(), &bob)
                .is_err()
        );
        let welcome = alice
            .create_and_invite(&bob.key_package().unwrap(), &bob)
            .unwrap();
        bob.join(&welcome, &alice).unwrap();
        assert!(bob.join(&welcome, &alice).is_err());
    }
}
