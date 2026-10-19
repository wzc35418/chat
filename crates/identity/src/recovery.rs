//! IDENTITY-013/014：身份恢复文件导出/导入。
//!
//! 为什么需要它：换机器/重装后必须能"把身份拿回来"。导出物是**加密的恢复文件**，
//! 口令派生用 argon2id（随机盐、参数版本化、参数白名单），加密用 XChaCha20-Poly1305。
//!
//! 安全边界：
//!
//! - 恢复文件里**只有身份材料**（账户/设备种子与对应公钥、设备列表摘要、创建时间）；
//!   **绝不含存储主密钥**，因此拿到恢复文件也不能解开旧库里的消息与附件；
//! - 口令错误与文件被篡改都必须显式失败（口令错误 → BAD_PASSPHRASE，篡改/结构损坏 → CORRUPTED）；
//! - 只允许导入**空库**（已有身份 → TARGET_NOT_EMPTY，绝不覆盖既有身份）；
//! - 文件上限 1 MiB；argon2 参数必须在白名单里（防止把 KDF 成本压到可暴力破解）。

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use chat_storage::formal::{self, Store};
use rand::RngExt;

use crate::keys::DeviceKeys;
use crate::service::LocalAccount;

/// 恢复文件字节上限（1 MiB）。
pub const MAX_RECOVERY_BUNDLE_BYTES: usize = 1024 * 1024;
/// 文件魔数（8 字节，自描述版本族）。
pub const RECOVERY_MAGIC: [u8; 8] = *b"chatreco";
/// 恢复文件格式版本。
pub const RECOVERY_FORMAT_VERSION: u8 = 1;
/// argon2 盐长度（16 字节）。
pub const RECOVERY_SALT_BYTES: usize = 16;
/// XChaCha20-Poly1305 nonce 长度。
pub const RECOVERY_NONCE_BYTES: usize = 24;
/// 口令校验值（KCV）长度：让"口令错误"与"文件被篡改"能被区分开。
pub const RECOVERY_KCV_BYTES: usize = 16;
/// 口令长度范围（UTF-8 字节）。
pub const MIN_PASSPHRASE_BYTES: usize = 8;
/// 口令长度上限。
pub const MAX_PASSPHRASE_BYTES: usize = 256;

/// 允许的 argon2id 参数（m_cost KiB, t_cost 轮, p_cost 并行度）。
///
/// 导入时只接受白名单里的组合：否则攻击者可以用极低的参数重新封装一个"恢复文件"，
/// 或者让本机在导入时被拖进超长 KDF。
pub const ALLOWED_ARGON2_PARAMS: [Argon2Params; 3] = [
    Argon2Params { m_cost_kib: 19_456, t_cost: 2, p_cost: 1 },
    Argon2Params { m_cost_kib: 47_104, t_cost: 1, p_cost: 1 },
    Argon2Params { m_cost_kib: 65_536, t_cost: 3, p_cost: 1 },
];

/// 一组 argon2id 参数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Argon2Params {
    /// 内存成本（KiB）。
    pub m_cost_kib: u32,
    /// 时间成本（轮数）。
    pub t_cost: u32,
    /// 并行度。
    pub p_cost: u32,
}

impl Argon2Params {
    /// 默认参数（白名单第一组）。
    pub const DEFAULT: Self = ALLOWED_ARGON2_PARAMS[0];

    /// 是否在白名单里。
    pub fn is_allowed(self) -> bool {
        ALLOWED_ARGON2_PARAMS.contains(&self)
    }
}

/// 恢复层稳定错误码（本模块自有码表，不改 PROTOCOL-014 冻结码）。
#[derive(Debug)]
pub enum RecoveryError {
    /// 口令错误（AEAD 认证失败但结构完整）。
    BadPassphrase,
    /// 文件结构损坏或被篡改。
    Corrupted,
    /// 版本/参数不受支持。
    Unsupported,
    /// 目标库已有身份，绝不覆盖。
    TargetNotEmpty,
    /// 口令/参数等输入非法。
    InvalidInput(String),
    /// 正式存储失败（存储冻结码原样透出）。
    Storage(chat_storage::formal::StorageError),
}

impl RecoveryError {
    /// 稳定错误码文本。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadPassphrase => "RECOVERY_BAD_PASSPHRASE",
            Self::Corrupted => "RECOVERY_CORRUPTED",
            Self::Unsupported => "RECOVERY_UNSUPPORTED",
            Self::TargetNotEmpty => "RECOVERY_TARGET_NOT_EMPTY",
            Self::InvalidInput(_) => "INVALID_INPUT",
            Self::Storage(error) => error.code(),
        }
    }

    /// 脱敏说明（不含口令、种子或文件内容）。
    pub fn detail(&self) -> String {
        match self {
            Self::BadPassphrase => "recovery passphrase does not match the bundle".to_owned(),
            Self::Corrupted => "recovery bundle is corrupted or tampered with".to_owned(),
            Self::Unsupported => "recovery bundle version or KDF parameters are not supported".to_owned(),
            Self::TargetNotEmpty => "the target store already holds an identity".to_owned(),
            Self::InvalidInput(detail) => detail.clone(),
            Self::Storage(error) => format!("{error}"),
        }
    }
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for RecoveryError {}

impl From<formal::StorageError> for RecoveryError {
    fn from(error: formal::StorageError) -> Self {
        Self::Storage(error)
    }
}

/// 恢复层返回类型。
pub type RecoveryResult<T> = core::result::Result<T, RecoveryError>;

/// 导入/导出后的公开投影（不含种子）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryIdentityView {
    /// 账户 ID。
    pub account_id: String,
    /// 设备 ID。
    pub device_id: String,
    /// 账户公钥（小写 hex）。
    pub account_public_key: String,
    /// 设备公钥（小写 hex）。
    pub device_public_key: String,
    /// 显示名。
    pub display_name: String,
    /// 创建时间（Unix 毫秒）。
    pub created_at_ms: i64,
    /// 设备列表摘要（小写 hex）。
    pub devices_digest: String,
}

/// 解密后的恢复载荷（**含种子**，只在本次调用内使用）。
#[derive(Serialize, Deserialize)]
struct RecoveryPayload {
    format_version: u8,
    account_id: String,
    device_id: String,
    display_name: String,
    created_at_ms: i64,
    account_public_key: String,
    device_public_key: String,
    devices_digest: String,
    account_seed: String,
    device_seed: String,
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

fn decode_hex32(text: &str, what: &str) -> RecoveryResult<Zeroizing<[u8; 32]>> {
    if text.len() != 64 {
        return Err(RecoveryError::Corrupted);
    }
    let bytes = text.as_bytes();
    let mut out = Zeroizing::new([0_u8; 32]);
    for index in 0..32 {
        let high = (bytes[index * 2] as char)
            .to_digit(16)
            .ok_or(RecoveryError::Corrupted)?;
        let low = (bytes[index * 2 + 1] as char)
            .to_digit(16)
            .ok_or(RecoveryError::Corrupted)?;
        out[index] = ((high as u8) << 4) | low as u8;
    }
    let _ = what;
    Ok(out)
}

fn validate_passphrase(passphrase: &str) -> RecoveryResult<()> {
    if passphrase.len() < MIN_PASSPHRASE_BYTES {
        return Err(RecoveryError::InvalidInput(format!(
            "recovery passphrase must be at least {MIN_PASSPHRASE_BYTES} bytes"
        )));
    }
    if passphrase.len() > MAX_PASSPHRASE_BYTES {
        return Err(RecoveryError::InvalidInput(format!(
            "recovery passphrase must not exceed {MAX_PASSPHRASE_BYTES} bytes"
        )));
    }
    if passphrase.chars().any(char::is_control) {
        return Err(RecoveryError::InvalidInput(
            "recovery passphrase must not contain control characters".to_owned(),
        ));
    }
    Ok(())
}

/// 设备列表摘要（本切片只有本机一台设备；域分隔 + 账户 + 设备）。
fn devices_digest(account_id: &str, device_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"chat/identity/recovery-devices/v1");
    hasher.update((account_id.len() as u32).to_be_bytes());
    hasher.update(account_id.as_bytes());
    hasher.update((device_id.len() as u32).to_be_bytes());
    hasher.update(device_id.as_bytes());
    hasher.finalize().into()
}

fn derive_key(passphrase: &str, salt: &[u8], params: Argon2Params) -> RecoveryResult<Zeroizing<[u8; 32]>> {
    if !params.is_allowed() {
        return Err(RecoveryError::Unsupported);
    }
    let argon = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(params.m_cost_kib, params.t_cost, params.p_cost, Some(32))
            .map_err(|_| RecoveryError::Unsupported)?,
    );
    let mut key = Zeroizing::new([0_u8; 32]);
    argon
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| RecoveryError::Unsupported)?;
    Ok(key)
}

/// 文件头里的绑定信息（AAD），防止头和正文被拼接篡改。
/// 口令校验值：H("recovery-kcv-v1" || 派生密钥) 的前 16 字节。
///
/// 它让导入能区分两种情况：KCV 不匹配 = 口令错（BAD_PASSPHRASE）；KCV 匹配但 AEAD 认证失败 =
/// 文件被篡改（CORRUPTED）。KCV 只泄露"这个口令派生出的密钥的固定摘要"，离线破解成本仍由
/// argon2id 决定（与直接猜 AEAD 标签等价）。
fn key_check_value(key: &[u8; 32]) -> [u8; RECOVERY_KCV_BYTES] {
    let mut hasher = Sha256::new();
    hasher.update(b"chat/identity/recovery-kcv/v1");
    hasher.update(key);
    let digest = hasher.finalize();
    let mut kcv = [0_u8; RECOVERY_KCV_BYTES];
    kcv.copy_from_slice(&digest[..RECOVERY_KCV_BYTES]);
    kcv
}

fn bundle_aad(version: u8, params: Argon2Params, salt: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(32);
    aad.extend_from_slice(&RECOVERY_MAGIC);
    aad.push(version);
    aad.extend_from_slice(&params.m_cost_kib.to_be_bytes());
    aad.extend_from_slice(&params.t_cost.to_be_bytes());
    aad.extend_from_slice(&params.p_cost.to_be_bytes());
    aad.extend_from_slice(salt);
    aad
}

/// IDENTITY-013：导出身份恢复文件（加密，**不含存储主密钥**）。
pub fn export_recovery_bundle(store: &Store, passphrase: &str) -> RecoveryResult<Vec<u8>> {
    export_recovery_bundle_with_params(store, passphrase, Argon2Params::DEFAULT)
}

/// 同上，但显式指定 argon2 参数（必须在白名单里）。
pub fn export_recovery_bundle_with_params(
    store: &Store,
    passphrase: &str,
    params: Argon2Params,
) -> RecoveryResult<Vec<u8>> {
    validate_passphrase(passphrase)?;
    if !params.is_allowed() {
        return Err(RecoveryError::Unsupported);
    }
    let seeds = store
        .load_identity_seeds()?
        .ok_or_else(|| RecoveryError::InvalidInput("the store holds no identity".to_owned()))?;
    let account = store
        .load_identity_account()?
        .ok_or_else(|| RecoveryError::InvalidInput("the store holds no identity".to_owned()))?;
    let account_seed: [u8; 32] = seeds
        .account_seed
        .as_slice()
        .try_into()
        .map_err(|_| RecoveryError::InvalidInput("stored account seed is not 32 bytes".to_owned()))?;
    let device_seed: [u8; 32] = seeds
        .device_seed
        .as_slice()
        .try_into()
        .map_err(|_| RecoveryError::InvalidInput("stored device seed is not 32 bytes".to_owned()))?;
    // 种子 → 公钥必须与库里的投影自洽（否则库已损坏）。
    let local = LocalAccount::from_seed(&seeds.display_name, u64::try_from(seeds.created_at_ms).unwrap_or(1), &account_seed)
        .map_err(|error| RecoveryError::InvalidInput(format!("{error}")))?;
    let device = DeviceKeys::from_seed(&device_seed);
    let account_public_key = *local.trust_anchor().account_public_key();
    let device_public_key = device.public_key();
    let device_id = device.device_id(local.account_id());
    if local.account_id().encode() != account.account_id || device_id.encode() != account.device_id {
        return Err(RecoveryError::Storage(formal::StorageError::StateCorrupted(
            "stored identity seeds do not derive the recorded ids".to_owned(),
        )));
    }
    let digest = devices_digest(&account.account_id, &account.device_id);
    let payload = RecoveryPayload {
        format_version: RECOVERY_FORMAT_VERSION,
        account_id: account.account_id.clone(),
        device_id: account.device_id.clone(),
        display_name: seeds.display_name.clone(),
        created_at_ms: seeds.created_at_ms,
        account_public_key: hex_lower(&account_public_key),
        device_public_key: hex_lower(&device_public_key),
        devices_digest: hex_lower(&digest),
        account_seed: hex_lower(&account_seed),
        device_seed: hex_lower(&device_seed),
    };
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&payload)
            .map_err(|_| RecoveryError::InvalidInput("recovery payload could not be encoded".to_owned()))?,
    );
    let mut salt = [0u8; RECOVERY_SALT_BYTES];
    rand::rng().fill(&mut salt);
    let key = derive_key(passphrase, &salt, params)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| RecoveryError::Unsupported)?;
    let mut nonce_bytes = [0u8; RECOVERY_NONCE_BYTES];
    rand::rng().fill(&mut nonce_bytes);
    let nonce = XNonce::from(nonce_bytes);
    let aad = bundle_aad(RECOVERY_FORMAT_VERSION, params, &salt);
    let ciphertext = cipher
        .encrypt(&nonce, Payload { msg: plaintext.as_slice(), aad: &aad })
        .map_err(|_| RecoveryError::Corrupted)?;
    let mut bundle = Vec::with_capacity(
        8 + 1 + 12 + RECOVERY_SALT_BYTES + RECOVERY_KCV_BYTES + RECOVERY_NONCE_BYTES
            + ciphertext.len(),
    );
    bundle.extend_from_slice(&RECOVERY_MAGIC);
    bundle.push(RECOVERY_FORMAT_VERSION);
    bundle.extend_from_slice(&params.m_cost_kib.to_be_bytes());
    bundle.extend_from_slice(&params.t_cost.to_be_bytes());
    bundle.extend_from_slice(&params.p_cost.to_be_bytes());
    bundle.extend_from_slice(&salt);
    bundle.extend_from_slice(&key_check_value(&key));
    bundle.extend_from_slice(&nonce_bytes);
    bundle.extend_from_slice(&ciphertext);
    if bundle.len() > MAX_RECOVERY_BUNDLE_BYTES {
        return Err(RecoveryError::InvalidInput(
            "recovery bundle exceeds the 1 MiB limit".to_owned(),
        ));
    }
    Ok(bundle)
}

/// IDENTITY-014：把恢复文件导入**空库**（绝不覆盖既有身份）。
pub fn import_recovery_bundle(
    store: &mut Store,
    bundle: &[u8],
    passphrase: &str,
) -> RecoveryResult<RecoveryIdentityView> {
    validate_passphrase(passphrase)?;
    if bundle.len() > MAX_RECOVERY_BUNDLE_BYTES {
        return Err(RecoveryError::InvalidInput(
            "recovery bundle exceeds the 1 MiB limit".to_owned(),
        ));
    }
    let header_len = 8
        + 1
        + 12
        + RECOVERY_SALT_BYTES
        + RECOVERY_KCV_BYTES
        + RECOVERY_NONCE_BYTES;
    if bundle.len() <= header_len || bundle.get(..8) != Some(RECOVERY_MAGIC.as_slice()) {
        return Err(RecoveryError::Corrupted);
    }
    let version = bundle[8];
    if version != RECOVERY_FORMAT_VERSION {
        return Err(RecoveryError::Unsupported);
    }
    let params = Argon2Params {
        m_cost_kib: u32::from_be_bytes([bundle[9], bundle[10], bundle[11], bundle[12]]),
        t_cost: u32::from_be_bytes([bundle[13], bundle[14], bundle[15], bundle[16]]),
        p_cost: u32::from_be_bytes([bundle[17], bundle[18], bundle[19], bundle[20]]),
    };
    if !params.is_allowed() {
        return Err(RecoveryError::Unsupported);
    }
    let mut salt = [0u8; RECOVERY_SALT_BYTES];
    salt.copy_from_slice(&bundle[21..21 + RECOVERY_SALT_BYTES]);
    let kcv_start = 21 + RECOVERY_SALT_BYTES;
    let mut stored_kcv = [0_u8; RECOVERY_KCV_BYTES];
    stored_kcv.copy_from_slice(&bundle[kcv_start..kcv_start + RECOVERY_KCV_BYTES]);
    let nonce_start = kcv_start + RECOVERY_KCV_BYTES;
    let mut nonce_bytes = [0u8; RECOVERY_NONCE_BYTES];
    nonce_bytes.copy_from_slice(&bundle[nonce_start..nonce_start + RECOVERY_NONCE_BYTES]);
    let ciphertext = &bundle[header_len..];
    let key = derive_key(passphrase, &salt, params)?;
    // 先比口令校验值：不匹配就是口令错（而不是"文件坏了"）。
    let mut diff = 0_u8;
    let derived_kcv = key_check_value(&key);
    for index in 0..RECOVERY_KCV_BYTES {
        diff |= derived_kcv[index] ^ stored_kcv[index];
    }
    if diff != 0 {
        return Err(RecoveryError::BadPassphrase);
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
        .map_err(|_| RecoveryError::Unsupported)?;
    let aad = bundle_aad(version, params, &salt);
    let plaintext = cipher
        .decrypt(&XNonce::from(nonce_bytes), Payload { msg: ciphertext, aad: &aad })
        .map_err(|_| RecoveryError::Corrupted)?;
    let plaintext = Zeroizing::new(plaintext);
    let payload: RecoveryPayload = serde_json::from_slice(plaintext.as_slice())
        .map_err(|_| RecoveryError::Corrupted)?;
    if payload.format_version != RECOVERY_FORMAT_VERSION {
        return Err(RecoveryError::Unsupported);
    }
    let account_seed = decode_hex32(&payload.account_seed, "account seed")?;
    let device_seed = decode_hex32(&payload.device_seed, "device seed")?;
    let account_key = decode_hex32(&payload.account_public_key, "account public key")?;
    let device_key = decode_hex32(&payload.device_public_key, "device public key")?;
    let created_at_ms = payload.created_at_ms;
    let local = LocalAccount::from_seed(
        &payload.display_name,
        u64::try_from(created_at_ms).unwrap_or(1),
        &*account_seed,
    )
    .map_err(|error| RecoveryError::InvalidInput(format!("{error}")))?;
    let device = DeviceKeys::from_seed(&*device_seed);
    // 内容自洽：种子派生的 ID/公钥必须与载荷声明一致（否则是被改写过的文件）。
    if local.account_id().encode() != payload.account_id
        || device.device_id(local.account_id()).encode() != payload.device_id
        || *local.trust_anchor().account_public_key() != *account_key
        || device.public_key() != *device_key
    {
        return Err(RecoveryError::Corrupted);
    }
    if hex_lower(&devices_digest(&payload.account_id, &payload.device_id)) != payload.devices_digest {
        return Err(RecoveryError::Corrupted);
    }
    // 目标库必须是没有身份的**空库**：绝不覆盖。
    if store.load_identity_account()?.is_some() || store.load_identity_seeds()?.is_some() {
        return Err(RecoveryError::TargetNotEmpty);
    }
    let input = formal::IdentityAccountInput {
        account_id: payload.account_id.clone(),
        device_id: payload.device_id.clone(),
        account_public_key: *account_key,
        device_public_key: *device_key,
        account_seed: account_seed.to_vec(),
        device_seed: device_seed.to_vec(),
        display_name: payload.display_name.clone(),
        fingerprint: *local.fingerprint().as_bytes(),
        created_at_ms,
    };
    store.save_identity_account(input)?;
    Ok(RecoveryIdentityView {
        account_id: payload.account_id,
        device_id: payload.device_id,
        account_public_key: payload.account_public_key,
        device_public_key: payload.device_public_key,
        display_name: payload.display_name,
        created_at_ms,
        devices_digest: payload.devices_digest,
    })
}
