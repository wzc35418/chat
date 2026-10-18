//! P0-04 存储主密钥：Argon2id 口令派生与 32 字节密钥文件加载。
//!
//! 约束（见 docs/implementation/crypto.md 与 storage.md）：
//! - 主密钥固定 32 字节，只以 chat-storage 的 ProtectedStorageKey 形式在可信核心路径
//!   内存在；Drop 时 zeroize，不进入普通 DTO 或日志；
//! - KDF 参数（内存成本、迭代次数、并行度、盐长度）全部有界，越界或畸形输入显式失败；
//! - 派生结果连同参数一起返回 KeyProvenance，写库后重开必须完全一致，否则拒绝打开。

use anyhow::{Context, Result, anyhow, ensure};
use argon2::{Algorithm, Argon2, Params, Version};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey};
use std::fs;
use std::path::Path;
use zeroize::Zeroizing;

/// 主密钥长度（字节），与 chat-storage 保持一致。
pub const MASTER_KEY_BYTES: usize = chat_storage::formal::MASTER_KEY_BYTES;
/// 盐长度下界（字节）。
pub const MIN_SALT_BYTES: usize = 16;
/// 盐长度上界（字节）。
pub const MAX_SALT_BYTES: usize = 64;
/// Argon2id 内存成本下界（KiB）。
pub const MIN_M_COST_KIB: u32 = 8 * 1024;
/// Argon2id 内存成本上界（KiB）。
pub const MAX_M_COST_KIB: u32 = 1024 * 1024;
/// Argon2id 迭代次数下界。
pub const MIN_T_COST: u32 = 1;
/// Argon2id 迭代次数上界。
pub const MAX_T_COST: u32 = 16;
/// Argon2id 并行度下界。
pub const MIN_PARALLELISM: u32 = 1;
/// Argon2id 并行度上界。
pub const MAX_PARALLELISM: u32 = 8;
/// 口令长度上界（字节）。
pub const MAX_PASSPHRASE_BYTES: usize = 1024;
/// 密钥文件允许的最大字节数（大于 32 直接拒绝，不做截断）。
pub const MAX_KEY_FILE_BYTES: u64 = 4096;

/// Argon2id 派生参数。盐必须是随机生成并随库保存，不能复用固定盐。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KdfParams {
    /// 内存成本（KiB）。
    pub m_cost_kib: u32,
    /// 迭代次数。
    pub t_cost: u32,
    /// 并行度。
    pub parallelism: u32,
    /// 盐（16..=64 字节）。
    pub salt: Vec<u8>,
}

impl KdfParams {
    /// 用随机盐和推荐参数（64 MiB / 3 次迭代 / 单线程）构造。
    pub fn recommended() -> Result<Self> {
        Self::with_salt(64 * 1024, 3, 1, new_salt()?)
    }

    /// 显式指定全部参数并做有界校验。
    pub fn with_salt(
        m_cost_kib: u32,
        t_cost: u32,
        parallelism: u32,
        salt: Vec<u8>,
    ) -> Result<Self> {
        let params = Self {
            m_cost_kib,
            t_cost,
            parallelism,
            salt,
        };
        params.validate()?;
        Ok(params)
    }

    /// 有界校验；任何越界都必须显式失败，而不是截断到合法范围。
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (MIN_M_COST_KIB..=MAX_M_COST_KIB).contains(&self.m_cost_kib),
            "argon2id m_cost_kib must be within {MIN_M_COST_KIB}..={MAX_M_COST_KIB}"
        );
        ensure!(
            (MIN_T_COST..=MAX_T_COST).contains(&self.t_cost),
            "argon2id t_cost must be within {MIN_T_COST}..={MAX_T_COST}"
        );
        ensure!(
            (MIN_PARALLELISM..=MAX_PARALLELISM).contains(&self.parallelism),
            "argon2id parallelism must be within {MIN_PARALLELISM}..={MAX_PARALLELISM}"
        );
        ensure!(
            (MIN_SALT_BYTES..=MAX_SALT_BYTES).contains(&self.salt.len()),
            "argon2id salt must be within {MIN_SALT_BYTES}..={MAX_SALT_BYTES} bytes"
        );
        Ok(())
    }

    fn to_provenance(&self) -> KeyProvenance {
        KeyProvenance::Argon2id {
            m_cost_kib: self.m_cost_kib,
            t_cost: self.t_cost,
            parallelism: self.parallelism,
            salt: self.salt.clone(),
        }
    }
}

/// 派生结果：主密钥 + 必须随库保存的派生来源记录。
pub struct DerivedMasterKey {
    /// 32 字节主密钥（Drop 时清零）。
    pub key: ProtectedStorageKey,
    /// 派生参数记录；重开存储时必须完全一致。
    pub provenance: KeyProvenance,
}

/// 生成 16 字节随机盐。
pub fn new_salt() -> Result<Vec<u8>> {
    ProtectedStorageKey::random_salt(MIN_SALT_BYTES).map_err(anyhow::Error::from)
}

/// 由口令派生 32 字节存储主密钥（Argon2id / v0x13）。
pub fn derive_master_key(passphrase: &[u8], params: &KdfParams) -> Result<DerivedMasterKey> {
    ensure!(!passphrase.is_empty(), "passphrase must not be empty");
    ensure!(
        passphrase.len() <= MAX_PASSPHRASE_BYTES,
        "passphrase exceeds {MAX_PASSPHRASE_BYTES} bytes"
    );
    params.validate()?;
    let argon_params = Params::new(
        params.m_cost_kib,
        params.t_cost,
        params.parallelism,
        Some(MASTER_KEY_BYTES),
    )
    .map_err(|error| anyhow!("invalid argon2id parameters: {error}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);
    let mut output = Zeroizing::new([0u8; MASTER_KEY_BYTES]);
    argon
        .hash_password_into(passphrase, &params.salt, output.as_mut())
        .map_err(|error| anyhow!("argon2id derivation failed: {error}"))?;
    Ok(DerivedMasterKey {
        key: ProtectedStorageKey::from_bytes(*output),
        provenance: params.to_provenance(),
    })
}

/// 从密钥文件加载 32 字节主密钥。长度不足/超出/缺失都必须显式失败，不做截断或补零。
pub fn load_master_key_file(path: &Path) -> Result<ProtectedStorageKey> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("master key file is not readable: {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "master key path must be a regular file: {}",
        path.display()
    );
    ensure!(
        metadata.len() <= MAX_KEY_FILE_BYTES,
        "master key file exceeds {MAX_KEY_FILE_BYTES} bytes"
    );
    ensure!(
        metadata.len() == MASTER_KEY_BYTES as u64,
        "master key file must contain exactly {MASTER_KEY_BYTES} bytes"
    );
    let bytes = Zeroizing::new(
        fs::read(path)
            .with_context(|| format!("cannot read master key file: {}", path.display()))?,
    );
    ensure!(
        bytes.len() == MASTER_KEY_BYTES,
        "master key file changed while being read"
    );
    ProtectedStorageKey::try_from_slice(bytes.as_slice()).map_err(anyhow::Error::from)
}
