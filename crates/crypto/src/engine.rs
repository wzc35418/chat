//! 真实 MLS（P0 设备对）。
//!
//! - 内存群操作：[\`MlsDevice\`] 的建群/加入/加解密；
//! - CRYPTO-004/010 持久化切片：[`MlsDevice::export_to_store`] /
//!   [`MlsDevice::import_from_store`] 把已提交群状态与 provider 秘密写入正式加密库
//!   （chat-storage 的 mls_group_state / mls_record 表），重启后可继续加解密；
//! - [`MlsDevice::advance_epoch_with_self_update`] 是“推进 epoch”的最小操作
//!   （本地自更新 commit）；对端处理该 commit 属于 CRYPTO-007，本轮未实现。
use anyhow::{Context, Result, bail, ensure};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

pub use chat_storage::formal::MlsGroupStateInput;
use chat_storage::formal::{MlsRecordInput, Store as FormalStore};

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

    /// 本设备的群键（MLS group id 的小写十六进制），用于正式存储的行标识。
    pub fn group_key(&self) -> Result<String> {
        Ok(hex_lower(self.group()?.group_id().as_slice()))
    }

    /// CRYPTO-004/010 持久化切片：把已提交群状态与 provider 秘密写入正式加密库。
    ///
    /// 一次调用是一笔 `WriteScope::mls_state` 事务：群状态行与全部 provider 记录
    /// 要么一起提交，要么一起回滚。epoch 由 epoch 单调校验保护——旧 epoch 的
    /// 快照不会覆盖新 epoch（返回 `STALE_EPOCH`）。
    pub fn export_to_store(&self, store: &mut FormalStore) -> Result<MlsExportSummary> {
        let group = self.group.as_ref().context("MLS group is not initialized")?;
        let epoch = group.epoch().as_u64();
        let group_id = group.group_id().as_slice().to_vec();
        let header = GroupHeader {
            name: self.credential.credential.serialized_content().to_vec(),
            signer_public: self.signer.public().to_vec(),
            group_id: group_id.clone(),
            epoch,
        };
        let state = encode_group_header(&header)?;
        let records = snapshot_provider_records(self)?;
        let record_count = records.len();
        let record = store
            .save_mls_group_state(MlsGroupStateInput {
                group_id: hex_lower(&group_id),
                epoch,
                state,
                records,
            })
            .map_err(anyhow::Error::new)?;
        Ok(MlsExportSummary {
            group_id: record.group_id,
            epoch: record.epoch,
            state_version: record.state_version,
            records: record_count,
        })
    }

    /// 从正式加密库恢复设备：读回群状态与 provider 记录后重建 provider 与群。
    ///
    /// 恢复出来的状态可继续加解密；任何密封记录被篡改/挪行、或被错误主密钥打开
    /// 都会显式失败（`STATE_CORRUPTED`），不会退化为“新建空群”。
    pub fn import_from_store(store: &FormalStore, group_key: &str) -> Result<Self> {
        let state = store
            .load_mls_group_state(group_key)
            .map_err(anyhow::Error::new)?
            .context("MLS group state is not persisted")?;
        let header = decode_group_header(&state.state)?;
        ensure!(
            header.epoch == state.epoch,
            "persisted MLS group header epoch does not match the stored epoch"
        );
        let records = store
            .load_mls_records(group_key)
            .map_err(anyhow::Error::new)?;
        ensure!(!records.is_empty(), "persisted MLS group has no records");

        let provider = OpenMlsRustCrypto::default();
        {
            let mut values = provider
                .storage()
                .values
                .write()
                .map_err(|_| anyhow::anyhow!("MLS provider storage lock is poisoned"))?;
            for record in records {
                values.insert(record.record_key, record.value);
            }
        }
        let signer = SignatureKeyPair::read(
            provider.storage(),
            &header.signer_public,
            SUITE.signature_algorithm(),
        )
        .context("persisted MLS signing key is missing")?;
        let credential = CredentialWithKey {
            credential: BasicCredential::new(header.name.clone()).into(),
            signature_key: signer.public().into(),
        };
        let group = MlsGroup::load(provider.storage(), &GroupId::from_slice(&header.group_id))?
            .context("persisted MLS group state is missing")?;
        ensure!(
            group.group_id().as_slice() == header.group_id.as_slice(),
            "persisted MLS group identity mismatch"
        );
        ensure!(
            group.epoch().as_u64() == header.epoch,
            "persisted MLS group epoch mismatch"
        );
        ensure!(
            signer.public() == header.signer_public.as_slice(),
            "persisted MLS signer does not match its public key"
        );
        Ok(Self {
            provider,
            signer,
            credential,
            group: Some(group),
        })
    }

    /// 推进 epoch 的最小操作：本地自更新 commit 并立即合并。
    ///
    /// 只用于本地状态推进/密钥轮换切片；对端处理该 commit（CRYPTO-007）与正式群
    /// 操作未实现，因此调用方必须自己负责把 commit 投递给成员。
    pub fn advance_epoch_with_self_update(&mut self) -> Result<u64> {
        let group = self
            .group
            .as_mut()
            .context("MLS group is not initialized")?;
        group
            .self_update(
                &self.provider,
                &self.signer,
                LeafNodeParameters::default(),
            )
            .context("stage MLS self update commit")?;
        group
            .merge_pending_commit(&self.provider)
            .context("merge MLS self update commit")?;
        Ok(group.epoch().as_u64())
    }
}


/// 持久化导出结果摘要（不含任何秘密）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MlsExportSummary {
    /// 正式存储里的群键（MLS group id 的十六进制）。
    pub group_id: String,
    /// 已提交 epoch。
    pub epoch: u64,
    /// 状态版本（每次成功写入 +1）。
    pub state_version: u64,
    /// 写入的 provider 记录条数。
    pub records: usize,
}

/// 导出格式版本（进入群状态明文）。
const EXPORT_FORMAT_VERSION: u8 = 1;
/// 显示名上限（字节），与 identity 的显示名上限一致。
const MAX_EXPORT_NAME_BYTES: usize = 256;
/// 签名公钥上限（字节）。
const MAX_EXPORT_KEY_BYTES: usize = 64;
/// MLS group id 上限（字节）。
const MAX_EXPORT_GROUP_ID_BYTES: usize = 64;

/// 群级头：恢复所需的公开/结构信息（记录本身的秘密在 provider 记录里）。
struct GroupHeader {
    name: Vec<u8>,
    signer_public: Vec<u8>,
    group_id: Vec<u8>,
    epoch: u64,
}

fn push_len_prefixed(out: &mut Vec<u8>, bytes: &[u8], limit: usize, label: &str) -> Result<()> {
    ensure!(
        bytes.len() <= limit && u16::try_from(bytes.len()).is_ok(),
        "{label} exceeds {limit} bytes"
    );
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn encode_group_header(header: &GroupHeader) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(96);
    out.push(EXPORT_FORMAT_VERSION);
    push_len_prefixed(&mut out, &header.name, MAX_EXPORT_NAME_BYTES, "MLS name")?;
    push_len_prefixed(
        &mut out,
        &header.signer_public,
        MAX_EXPORT_KEY_BYTES,
        "MLS signature key",
    )?;
    push_len_prefixed(
        &mut out,
        &header.group_id,
        MAX_EXPORT_GROUP_ID_BYTES,
        "MLS group ID",
    )?;
    out.extend_from_slice(&header.epoch.to_be_bytes());
    Ok(out)
}

fn decode_group_header(bytes: &[u8]) -> Result<GroupHeader> {
    ensure!(
        !bytes.is_empty() && bytes[0] == EXPORT_FORMAT_VERSION,
        "unsupported MLS export format version"
    );
    let mut cursor = 1usize;
    let take = |cursor: &mut usize, limit: usize, label: &str| -> Result<Vec<u8>> {
        ensure!(
            bytes.len() >= *cursor + 2,
            "persisted MLS header is truncated ({label})"
        );
        let length = usize::from(u16::from_be_bytes([bytes[*cursor], bytes[*cursor + 1]]));
        *cursor += 2;
        ensure!(length <= limit, "persisted MLS {label} exceeds {limit} bytes");
        ensure!(
            bytes.len() >= *cursor + length,
            "persisted MLS header is truncated ({label})"
        );
        let value = bytes[*cursor..*cursor + length].to_vec();
        *cursor += length;
        Ok(value)
    };
    let name = take(&mut cursor, MAX_EXPORT_NAME_BYTES, "name")?;
    let signer_public = take(&mut cursor, MAX_EXPORT_KEY_BYTES, "signature key")?;
    let group_id = take(&mut cursor, MAX_EXPORT_GROUP_ID_BYTES, "group ID")?;
    ensure!(
        bytes.len() == cursor + 8,
        "persisted MLS header has an invalid length"
    );
    let mut epoch_bytes = [0u8; 8];
    epoch_bytes.copy_from_slice(&bytes[cursor..cursor + 8]);
    ensure!(!name.is_empty(), "persisted MLS name is empty");
    ensure!(!signer_public.is_empty(), "persisted MLS key is empty");
    ensure!(!group_id.is_empty(), "persisted MLS group ID is empty");
    Ok(GroupHeader {
        name,
        signer_public,
        group_id,
        epoch: u64::from_be_bytes(epoch_bytes),
    })
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    text
}

/// 把 provider 内存存储整体快照成记录集（按记录键排序，保证确定性）。
fn snapshot_provider_records(device: &MlsDevice) -> Result<Vec<MlsRecordInput>> {
    let values = device
        .provider
        .storage()
        .values
        .read()
        .map_err(|_| anyhow::anyhow!("MLS provider storage lock is poisoned"))?;
    let mut records: Vec<MlsRecordInput> = values
        .iter()
        .map(|(record_key, value)| MlsRecordInput {
            record_key: record_key.clone(),
            value: value.clone(),
        })
        .collect();
    records.sort_by(|left, right| left.record_key.cmp(&right.record_key));
    Ok(records)
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
