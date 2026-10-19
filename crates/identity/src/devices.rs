//! IDENTITY-011/012：设备登记与撤销集合（正式库落库形态）。
//!
//! 语义：
//!
//! - `register_device` 幂等 upsert：同设备重复登记只刷新公钥/标签，**不会复活已撤销设备**；
//! - `list_devices` 默认隐藏已撤销行，`include_revoked = true` 时连历史行一起返回；
//! - `revoke_device` 是单向的；**不能撤销本机**（→ `DEVICE_SELF_REVOKE`），跨账户撤销显式拒绝；
//! - `device_is_revoked` 供恢复/信任判断使用（不存在视为"未撤销"）。

use chat_storage::formal::{
    DEVICE_PUBLIC_KEY_BYTES, DeviceRegistrationInput, DeviceRegistryRecord, Store,
};

use crate::error::IdentityError;

/// 设备集合层稳定错误码。
#[derive(Debug)]
pub enum DeviceError {
    /// 不能撤销本机设备（必须走"换机/恢复"流程）。
    SelfRevoke,
    /// 输入不合法（账户/设备不匹配等）。
    InvalidInput(String),
    /// 身份层错误（原样透出）。
    Identity(IdentityError),
    /// 存储层错误（原样透出）。
    Storage(chat_storage::formal::StorageError),
}

impl DeviceError {
    /// 稳定错误码文本。
    pub fn code(&self) -> &'static str {
        match self {
            Self::SelfRevoke => "DEVICE_SELF_REVOKE",
            Self::InvalidInput(_) => "INVALID_INPUT",
            Self::Identity(error) => error.code().as_str(),
            Self::Storage(error) => error.code(),
        }
    }

    /// 脱敏说明。
    pub fn detail(&self) -> String {
        match self {
            Self::SelfRevoke => {
                "the local device cannot revoke itself; use recovery instead".to_owned()
            }
            Self::InvalidInput(detail) => detail.clone(),
            Self::Identity(error) => format!("{error:?}"),
            Self::Storage(error) => format!("{error}"),
        }
    }
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for DeviceError {}

impl From<IdentityError> for DeviceError {
    fn from(error: IdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<chat_storage::formal::StorageError> for DeviceError {
    fn from(error: chat_storage::formal::StorageError) -> Self {
        Self::Storage(error)
    }
}

/// 设备集合层返回类型。
pub type DeviceResult<T> = core::result::Result<T, DeviceError>;

/// 一台设备的公开投影（公钥以 hex 暴露，它不是秘密）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceEntry {
    /// 设备 ID（PROTOCOL-001 文本）。
    pub device_id: String,
    /// 所属账户 ID。
    pub account_id: String,
    /// 设备签名公钥（小写 hex，32 字节）。
    pub device_public_key: String,
    /// 本地标签。
    pub label: String,
    /// 登记时间（Unix 毫秒）。
    pub added_at_ms: i64,
    /// 撤销时间（`None` = 未撤销）。
    pub revoked_at_ms: Option<i64>,
    /// 撤销原因。
    pub revoked_reason: Option<String>,
}

impl DeviceEntry {
    /// 是否已撤销。
    pub fn is_revoked(&self) -> bool {
        self.revoked_at_ms.is_some()
    }
}

/// 登记一台设备（幂等）。
pub fn register_device(
    store: &mut Store,
    device_id: &str,
    account_id: &str,
    device_public_key: [u8; DEVICE_PUBLIC_KEY_BYTES],
    label: &str,
) -> DeviceResult<DeviceEntry> {
    if device_id.is_empty() || account_id.is_empty() {
        return Err(DeviceError::InvalidInput(
            "device id and account id must not be empty".to_owned(),
        ));
    }
    let input = DeviceRegistrationInput {
        device_id: device_id.to_owned(),
        account_id: account_id.to_owned(),
        device_public_key,
        label: label.to_owned(),
    };
    let record = store.upsert_identity_device(&input)?;
    if record.account_id != account_id {
        return Err(DeviceError::InvalidInput(
            "the device is already registered to another account".to_owned(),
        ));
    }
    Ok(entry_of(&record))
}

/// 列出一个账户的设备（默认隐藏已撤销行）。
pub fn registered_devices(
    store: &Store,
    account_id: &str,
    include_revoked: bool,
) -> DeviceResult<Vec<DeviceEntry>> {
    Ok(store
        .list_identity_devices(account_id, include_revoked)?
        .iter()
        .map(entry_of)
        .collect())
}

/// 撤销一台已登记的库内设备（幂等；不能撤销本机）。
pub fn revoke_registered_device(
    store: &mut Store,
    account_id: &str,
    device_id: &str,
    local_device_id: &str,
    reason: &str,
    now_ms: i64,
) -> DeviceResult<bool> {
    if device_id == local_device_id {
        return Err(DeviceError::SelfRevoke);
    }
    let Some(record) = store.load_identity_device(device_id)? else {
        return Err(DeviceError::InvalidInput(
            "the device is not registered".to_owned(),
        ));
    };
    if record.account_id != account_id {
        return Err(DeviceError::InvalidInput(
            "the device does not belong to this account".to_owned(),
        ));
    }
    Ok(store.revoke_identity_device(device_id, reason, now_ms)?)
}

/// 设备是否已撤销（未登记视为未撤销）。
pub fn device_is_revoked(store: &Store, device_id: &str) -> DeviceResult<bool> {
    Ok(store.identity_device_is_revoked(device_id)?)
}

/// 单台设备的登记记录（未登记 → `None`）。
pub fn load_device(store: &Store, device_id: &str) -> DeviceResult<Option<DeviceEntry>> {
    Ok(store.load_identity_device(device_id)?.as_ref().map(entry_of))
}

fn entry_of(record: &DeviceRegistryRecord) -> DeviceEntry {
    DeviceEntry {
        device_id: record.device_id.clone(),
        account_id: record.account_id.clone(),
        device_public_key: record
            .device_public_key
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        label: record.label.clone(),
        added_at_ms: record.added_at_ms,
        revoked_at_ms: record.revoked_at_ms,
        revoked_reason: record.revoked_reason.clone(),
    }
}

/// 恢复/信任路径：确认库里的正式账户与种子仍在（避免在空库上误判设备状态）。
pub fn local_identity_present(store: &Store) -> DeviceResult<bool> {
    Ok(store.load_identity_account()?.is_some() && store.load_identity_seeds()?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_revoke_is_a_stable_code() {
        assert_eq!(DeviceError::SelfRevoke.code(), "DEVICE_SELF_REVOKE");
        assert_eq!(
            DeviceError::InvalidInput("x".to_owned()).code(),
            "INVALID_INPUT"
        );
    }
}
