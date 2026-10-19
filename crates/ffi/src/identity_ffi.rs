//! 身份层的 UniFFI 对象：Kotlin 侧「口令加好友」竖直切片。
//!
//! 本对象把 [chat_identity] 的账户、邀请口令与联系人状态机投影成 UniFFI 对象，
//! 按 UI 约定暴露两类入口：
//!
//! - **进程内身份**（`create`）：只在内存里生成账户/设备密钥，不碰数据库；
//! - **持久身份**（`open_persisted`）：打开正式加密存储（chat-core 的
//!   `ConversationCore`，与 LocalStoreFfi 同一个库），恢复账户/设备种子并载入联系人；
//!   首次打开时生成新账户并落库；`accept_invite`/`confirm_identity_change` 通过
//!   `ConversationCore::upsert_contact` 把联系人写回；`shutdown` 关闭 core（幂等）。
//!
//! 两种构造共用一个公开面：`account / create_invite / preview_invite / accept_invite /
//! contacts / confirm_identity_change / shutdown`。
//!
//! 与既有 `local_store.rs`（正式加密存储对象）不共享状态，但**复用同一个**
//! [`FfiError`]：`code` 直接取 identity / core 的稳定错误码（INVALID_TARGET /
//! INVALID_SIGNATURE / CURSOR_EXPIRED / INVALID_STATE_TRANSITION / CONTENT_TOO_LARGE /
//! STATE_CORRUPTED …），Kotlin 侧只需要 `catch (e: FfiException.Store)` 一处。
//!
//! 安全边界（与 docs/implementation/identity.md 的 IDENTITY-001/004/005/006/007 对应）：
//!
//! - 口令只在验签 + 期限 + 绑定校验通过后才被解释；本对象不提供「先解析、后验签」
//!   的第二套入口，`preview_invite` 与 `accept_invite` 共用同一条校验链；
//! - 自己签发的口令不能当成联系人：目标账户等于本机账户时显式 INVALID_TARGET；
//! - `accept_invite` 要求口令绑定的节点与本对象创建时传入的节点一致，否则
//!   INVALID_TARGET（`preview_invite` 只把节点当提示展示）；
//! - 接受邀请只产生 `requested` 联系人（`can_send == false`），绝不自动信任；
//! - `identity_changed` 只能由显式 `confirm_identity_change` 复位，普通接受无法绕过；
//! - 持久路径以**库为准**：种子从受保护存储读回，派生结果与库中投影不一致时显式
//!   STATE_CORRUPTED，绝不降级成「新身份」；联系人先落库、成功后才提交到内存；
//! - 时间由内部时钟提供（生产 = 系统 UTC 毫秒；测试注入固定时钟）；
//! - 不发网络请求：一次性消费、outbox 与事件流由后续 core 集成补齐。
//!
//! 线程安全：UniFFI 要求对象可跨线程共享，因此内部状态用 `std::sync::Mutex`
//! 串行化；锁中毒时带返回值的调用显式返回 `CORE_PANICKED`。

use std::sync::{Arc, Mutex, MutexGuard};

use chat_core::service::{
    ContactUpsertRequest, ContactView as CoreContactView, ConversationCore, IdentityAccountInput,
};
use chat_identity::{
    AccountKeys, Contact, ContactState, DeviceKeys, Fingerprint, IdentityError, InviteExpectations,
    InviteScope, KEY_SEED_BYTES, LocalAccount, MAX_DISPLAY_NAME_BYTES,
};
use chat_protocol::{AccountId, CoordinatorId, DeviceId, ErrorCode};

use crate::local_store::FfiError;

/// FFI 层错误原因的字符上限（与 core 的脱敏规则一致）。
const MAX_REASON_CHARS: usize = 300;

/// 脱敏错误原因：截断到 300 字符并把换行压平，不把长输入或密钥材料塞进 UI。
fn sanitize(reason: &str) -> String {
    let truncated: String = reason.chars().take(MAX_REASON_CHARS).collect();
    truncated.replace('\n', " ")
}

/// 用 PROTOCOL-014 冻结码构造稳定错误。
fn store_error(code: ErrorCode, reason: impl AsRef<str>) -> FfiError {
    FfiError::Store {
        code: code.as_str().to_owned(),
        reason: sanitize(reason.as_ref()),
    }
}

/// 内核层状态错误（`CORE_CLOSED` / `CORE_PANICKED` 不在协议码表内，沿用 core 的既有字面量）。
fn state_error(code: &str, reason: &str) -> FfiError {
    FfiError::Store {
        code: code.to_owned(),
        reason: sanitize(reason),
    }
}

/// 把 core 的 `anyhow::Error` 投影成 FFI 错误；本 crate 不需要依赖 `anyhow`，
/// 由宏在调用点按类型推断完成（与 local_store.rs 的 failure! 同一做法）。
macro_rules! core_failure {
    ($error:expr) => {{
        let (code, reason) = chat_core::service::error_projection(&$error);
        FfiError::Store {
            code: code.to_owned(),
            reason: sanitize(&reason),
        }
    }};
}

impl From<IdentityError> for FfiError {
    /// identity 的 `Display` 只含类型化原因与数字上限，不含密钥、口令正文或用户内容；
    /// `code` 直接取 identity 冻结的映射表，UI 只依赖 `code`。
    fn from(error: IdentityError) -> Self {
        store_error(error.code(), error.to_string())
    }
}

/// 本机账户的脱敏视图（不含任何密钥材料）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AccountView {
    /// 账户 ID（`acct:1:<64 位小写 hex>`）。
    pub account_id: String,
    /// 本机设备 ID（`dev:1:<64 位小写 hex>`）。
    pub device_id: String,
    /// 显示名（只作展示，不能用于验证身份）。
    pub display_name: String,
    /// 人工核验指纹：16 组 4 位大写 hex、单空格分隔（79 字符）。
    pub fingerprint: String,
}

/// 刚生成的邀请口令及其公开摘要。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InviteView {
    /// 口令原文（`inv1.<hex 正文>.<hex 签名>`）；只在用户主动复制时展示，不得进日志。
    pub token: String,
    /// 邀请 ID：口令正文里 16 字节 nonce 的小写 hex（32 字符）。
    pub invite_id: String,
    /// 失效时间（Unix 毫秒）。
    pub expires_at_ms: u64,
    /// 签发者账户 ID。
    pub account_id: String,
    /// 签发者设备 ID。
    pub device_id: String,
    /// 签发者人工核验指纹（分组展示）。
    pub fingerprint: String,
}

/// 校验通过后的邀请摘要（即将添加的联系人是谁、属于哪个节点、何时失效）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InviteSummary {
    /// 邀请 ID（32 位小写 hex）。
    pub invite_id: String,
    /// 签发者账户 ID。
    pub account_id: String,
    /// 签发者设备 ID。
    pub device_id: String,
    /// 签发者人工核验指纹（分组展示）。
    pub fingerprint: String,
    /// 口令绑定的协调节点（`node:1:<64 位小写 hex>`）。
    pub node: String,
    /// 失效时间（Unix 毫秒）。
    pub expires_at_ms: u64,
}

/// 联系人视图（本地记录，不是对端自述）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ContactView {
    /// 本地联系人句柄：当前等于 `account_id`（邀请口令不携带独立请求 ID），
    /// 必须原样回传给 `confirm_identity_change`。
    pub contact_id: String,
    /// 对端账户 ID。
    pub account_id: String,
    /// 本地备注/本地标签；邀请口令不携带对方昵称，未知时为空字符串（UI 自行回退）。
    pub display_name: String,
    /// 已固定的信任锚指纹；`requested` 时是收到的观测指纹。
    pub fingerprint: String,
    /// 稳定状态名：`requested` | `accepted` | `declined` | `blocked` | `identity_changed`。
    pub state: String,
    /// 是否允许发送：只有 `accepted` 且观测指纹与信任锚一致时为 true。
    pub can_send: bool,
}

/// 内部时钟：生产是系统 UTC 毫秒；测试注入固定时钟，避免在公开面留测试开关。
type Clock = fn() -> u64;

/// 系统 UTC 毫秒。时钟早于 1970 时返回 0，由 `LocalAccount::create` 按
/// `InvalidTimestamp` 显式拒绝（不静默改用其它时间）。
fn system_now_ms() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}

/// 一条本地联系人记录：状态机 + 本地备注。
#[derive(Clone)]
struct ContactRecord {
    contact: Contact,
    display_name: String,
    /// 从库中恢复、但 identity 的 `Contact` 无法直接表达的状态（目前只有
    /// `identity_changed`）：只用于上报给 UI 和「显式重新确认」；发送判定仍取
    /// `Contact::can_send()`（该状态恒为 false）。
    restored_state: Option<ContactState>,
}

impl ContactRecord {
    fn fresh(contact: Contact, display_name: String) -> Self {
        Self {
            contact,
            display_name,
            restored_state: None,
        }
    }

    /// 上报给 UI 的状态：库中恢复的状态优先，其余取状态机当前值。
    fn effective_state(&self) -> ContactState {
        self.restored_state.unwrap_or_else(|| self.contact.state())
    }
}

/// 对象内部状态。`account`/`device`/`core` 持有密钥与存储句柄，`shutdown` 后置空。
struct IdentityState {
    account: Option<LocalAccount>,
    device: Option<DeviceKeys>,
    account_id: AccountId,
    node: CoordinatorId,
    /// 公开投影快照（不含密钥，`shutdown` 后仍可读）。
    view: AccountView,
    contacts: Vec<ContactRecord>,
    /// 正式加密存储句柄；只有 `open_persisted` 构造的对象才有。
    core: Option<ConversationCore>,
    /// 持久化模式的库路径与主密钥（恢复文件导出/导入需要自行重开库）。
    database: Option<String>,
    master_key_hex: Option<String>,
    clock: Clock,
    closed: bool,
}

impl IdentityState {
    /// 密钥相关操作的前置检查：关闭后一律显式失败。
    fn require_open(&self) -> Result<(), FfiError> {
        if self.closed {
            return Err(state_error(
                "CORE_CLOSED",
                "identity object has been shut down",
            ));
        }
        Ok(())
    }

    /// 取长期账户密钥与设备密钥；任一已释放即视为已关闭。
    fn keys(&self) -> Result<(&LocalAccount, &DeviceKeys), FfiError> {
        match (&self.account, &self.device) {
            (Some(account), Some(device)) => Ok((account, device)),
            _ => Err(state_error(
                "CORE_CLOSED",
                "identity key material has been released",
            )),
        }
    }

    /// 关闭：释放账户/设备密钥并关闭正式存储句柄（core.close 自身幂等）。
    ///
    /// `account()`/`contacts()` 没有 `Result` 可返回，因此公开投影保留为关闭前快照；
    /// 需要密钥的操作一律返回 `CORE_CLOSED`。
    fn close(&mut self) {
        self.closed = true;
        self.account = None;
        self.device = None;
        if let Some(core) = self.core.take() {
            core.close();
        }
    }
}


/// 恢复文件导入结果（纯数据，不含种子）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RecoveryImportViewFfi {
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

/// 恢复层错误 → FFI 错误（稳定码原样透出：`RECOVERY_BAD_PASSPHRASE` 等）。
fn recovery_failure(error: chat_identity::recovery::RecoveryError) -> FfiError {
    let detail = error.detail();
    state_error(error.code(), &detail)
}

/// 只为恢复文件打开正式库（调用方必须已释放别的写句柄）。
fn open_formal_store(
    database_path: &str,
    master_key_hex: &str,
) -> Result<chat_storage::formal::Store, FfiError> {
    let key = decode_master_key_hex(master_key_hex)?;
    chat_storage::formal::open_store(
        chat_storage::formal::StoreOptions::new(database_path, chat_storage::formal::KeyProvenance::Raw32),
        chat_storage::formal::ProtectedStorageKey::from_bytes(key),
    )
    .map_err(|error| {
        let detail = error.to_string();
        state_error(error.code(), &detail)
    })
}

fn decode_master_key_hex(text: &str) -> Result<[u8; 32], FfiError> {
    if text.len() != 64 {
        return Err(state_error(
            "INVALID_INPUT",
            "master key must be exactly 64 hexadecimal characters",
        ));
    }
    let bytes = text.as_bytes();
    let mut key = [0_u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2])?;
        let low = hex_nibble(bytes[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Ok(key)
}

fn hex_nibble(byte: u8) -> Result<u8, FfiError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(state_error("INVALID_INPUT", "master key must be hexadecimal")),
    }
}

/// 重新打开正式库并恢复身份句柄（导出临时释放写锁后调用）。
fn restore_persisted(
    state: &mut IdentityState,
    database_path: &str,
    master_key_hex: &str,
) -> Result<(), FfiError> {
    let core = ConversationCore::open(database_path, master_key_hex)
        .map_err(|error| core_failure!(error))?;
    let persisted = core
        .open_persisted_identity()
        .map_err(|error| core_failure!(error))?
        .ok_or_else(|| state_error("STATE_CORRUPTED", "identity account is missing"))?;
    let listed = core.list_contacts().map_err(|error| core_failure!(error))?;
    let mut contacts = Vec::with_capacity(listed.len());
    for item in listed {
        contacts.push(restore_contact(item)?);
    }
    let projection = core
        .load_identity_account()
        .map_err(|error| core_failure!(error))?
        .ok_or_else(|| state_error("STATE_CORRUPTED", "identity projection is missing"))?;
    let view = AccountView {
        account_id: projection.account_id,
        device_id: projection.device_id,
        display_name: projection.display_name,
        fingerprint: projection.fingerprint,
    };
    state.account_id = persisted.account.account_id();
    state.account = Some(persisted.account);
    state.device = Some(persisted.device);
    state.contacts = contacts;
    state.view = view;
    state.core = Some(core);
    Ok(())
}

/// 设备集合里的一台设备（纯数据：公钥是公开材料，不含任何私钥）。
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DeviceViewFfi {
    /// 设备 ID（PROTOCOL-001 文本）。
    pub device_id: String,
    /// 所属账户 ID。
    pub account_id: String,
    /// 本地标签。
    pub label: String,
    /// 设备签名公钥（小写 hex，32 字节）。
    pub device_public_key_hex: String,
    /// 登记时间（Unix 毫秒）。
    pub added_at_ms: i64,
    /// 撤销时间（`None` = 未撤销）。
    pub revoked_at_ms: Option<i64>,
    /// 撤销原因。
    pub revoked_reason: Option<String>,
    /// 是否就是本机设备。
    pub is_local: bool,
}

/// 释放写锁期间需要用到的本机身份坐标（避免闭包里再借用 state）。
struct LocalDeviceContext {
    account_id: String,
    device_id: String,
    display_name: String,
    public_key: Option<[u8; 32]>,
    now_ms: u64,
}

/// 设备集合错误 → FFI 错误（稳定码原样透出：`DEVICE_SELF_REVOKE` 等）。
fn device_failure(error: chat_identity::devices::DeviceError) -> FfiError {
    let detail = error.detail();
    state_error(error.code(), &detail)
}

fn storage_failure(error: chat_storage::formal::StorageError) -> FfiError {
    let detail = error.to_string();
    state_error(error.code(), &detail)
}

/// 口令加好友与本机身份的 UniFFI 对象。
#[derive(uniffi::Object)]
pub struct IdentityFfi {
    state: Mutex<IdentityState>,
}

impl IdentityFfi {
    fn lock(&self) -> Result<MutexGuard<'_, IdentityState>, FfiError> {
        self.state.lock().map_err(|_| {
            state_error(
                "CORE_PANICKED",
                "identity lock is poisoned by an earlier panic",
            )
        })
    }

    /// 无 `Result` 可返回的公开投影：锁中毒时尽力读取当前值（不谎称成功，也不丢数据）。
    fn read(&self) -> MutexGuard<'_, IdentityState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// 用给定密钥材料组装对象（生产构造与测试共用）。
    fn assemble(
        node: CoordinatorId,
        account: LocalAccount,
        device: DeviceKeys,
        device_id: DeviceId,
        contacts: Vec<ContactRecord>,
        core: Option<ConversationCore>,
        database: Option<String>,
        master_key_hex: Option<String>,
        clock: Clock,
    ) -> Self {
        let account_id = account.account_id();
        let view = account_view_of(&account, device_id);
        Self {
            state: Mutex::new(IdentityState {
                account: Some(account),
                device: Some(device),
                account_id,
                node,
                view,
                contacts,
                core,
                database,
                master_key_hex,
                clock,
                closed: false,
            }),
        }
    }

    /// 进程内身份：随机密钥 + 无数据库（`create`）。
    fn create_with_clock(
        display_name: String,
        node: String,
        clock: Clock,
    ) -> Result<Arc<Self>, FfiError> {
        let node = parse_node(&node)?;
        let now_ms = clock();
        let account = LocalAccount::create(&display_name, now_ms).map_err(FfiError::from)?;
        let device = DeviceKeys::generate();
        let device_id = device.device_id(account.account_id());
        Ok(Arc::new(Self::assemble(
            node,
            account,
            device,
            device_id,
            Vec::new(),
            None,
            None,
            None,
            clock,
        )))
    }

    /// 由固定种子建立身份（只用于测试，用来核对 identity 的冻结派生向量）。
    #[cfg(test)]
    fn from_seeds(
        display_name: &str,
        node: &str,
        account_seed: u8,
        device_seed: u8,
        clock: Clock,
    ) -> Result<Arc<Self>, FfiError> {
        let node = parse_node(node)?;
        let now_ms = clock();
        let account =
            LocalAccount::from_seed(display_name, now_ms, &[account_seed; KEY_SEED_BYTES])
                .map_err(FfiError::from)?;
        let device = DeviceKeys::from_seed(&[device_seed; KEY_SEED_BYTES]);
        let device_id = device.device_id(account.account_id());
        Ok(Arc::new(Self::assemble(
            node,
            account,
            device,
            device_id,
            Vec::new(),
            None,
            None,
            None,
            clock,
        )))
    }

    /// 持久身份：打开正式加密存储，恢复或首次生成账户，并载入联系人。
    fn open_persisted_with_clock(
        database_path: String,
        master_key_hex: String,
        node: String,
        clock: Clock,
    ) -> Result<Arc<Self>, FfiError> {
        let node = parse_node(&node)?;
        let mut core = ConversationCore::open(&database_path, &master_key_hex)
            .map_err(|error| core_failure!(error))?;
        let existing = core
            .open_persisted_identity()
            .map_err(|error| core_failure!(error))?;
        if existing.is_none() {
            save_fresh_account(&mut core, clock)?;
        }
        // 回读：账户句柄与公开投影都以库为准，不用内存里的临时值。
        let persisted = core
            .open_persisted_identity()
            .map_err(|error| core_failure!(error))?
            .ok_or_else(|| {
                state_error(
                    "STATE_CORRUPTED",
                    "identity account is missing after it was saved",
                )
            })?;
        let projection = core
            .load_identity_account()
            .map_err(|error| core_failure!(error))?
            .ok_or_else(|| {
                state_error(
                    "STATE_CORRUPTED",
                    "identity projection is missing after it was saved",
                )
            })?;
        let derived = account_view_of(&persisted.account, persisted.device_id);
        let stored = AccountView {
            account_id: projection.account_id,
            device_id: projection.device_id,
            display_name: projection.display_name,
            fingerprint: projection.fingerprint,
        };
        if derived != stored {
            return Err(state_error(
                "STATE_CORRUPTED",
                "stored identity projection does not match the protected seeds",
            ));
        }
        let listed = core.list_contacts().map_err(|error| core_failure!(error))?;
        let mut contacts = Vec::with_capacity(listed.len());
        for item in listed {
            contacts.push(restore_contact(item)?);
        }
        Ok(Arc::new(Self::assemble(
            node,
            persisted.account,
            persisted.device,
            persisted.device_id,
            contacts,
            Some(core),
            Some(database_path),
            Some(master_key_hex),
            clock,
        )))
    }

    /// 释放正式库的独占写锁执行一段设备集合操作，结束后无条件把身份句柄装回去。
    ///
    /// 设备集合在 storage 层（identity_devices），而身份对象以独占写者身份持有同一个库；
    /// 因此这里采用"短暂释放 → 操作 → 恢复"的时序（与恢复文件导出同一套做法）。
    fn with_store<T>(
        &self,
        f: impl FnOnce(&mut chat_storage::formal::Store, &LocalDeviceContext) -> Result<T, FfiError>,
    ) -> Result<T, FfiError> {
        let mut state = self.lock()?;
        state.require_open()?;
        let Some(database) = state.database.clone() else {
            return Err(state_error(
                "INVALID_STATE_TRANSITION",
                "device management requires the persistent identity (open_persisted)",
            ));
        };
        let Some(master_key_hex) = state.master_key_hex.clone() else {
            return Err(state_error(
                "INVALID_STATE_TRANSITION",
                "device management requires the persistent identity (open_persisted)",
            ));
        };
        let local = LocalDeviceContext {
            account_id: state.view.account_id.clone(),
            device_id: state.view.device_id.clone(),
            display_name: state.view.display_name.clone(),
            public_key: state.device.as_ref().map(|device| device.public_key()),
            now_ms: (state.clock)(),
        };
        // 释放独占写锁。
        if let Some(core) = state.core.take() {
            core.close();
        }
        let outcome = (|| -> Result<T, FfiError> {
            let mut store = open_formal_store(&database, &master_key_hex)?;
            f(&mut store, &local)
        })();
        // 无论成功失败都把身份句柄装回去（否则对象会在释放写锁后不可用）。
        let restored = restore_persisted(&mut state, &database, &master_key_hex);
        match (outcome, restored) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

}

/// `node` 必须是 PROTOCOL-001 规范文本；解析失败按 INVALID_ID 显式拒绝。
fn parse_node(node: &str) -> Result<CoordinatorId, FfiError> {
    CoordinatorId::parse(node).map_err(|error| store_error(ErrorCode::InvalidId, error.to_string()))
}

/// 本地联系人备注：邀请口令不携带对方昵称，空串表示未知（UI 自行回退）。
///
/// 非空时按 identity 的显示名规则限边界（1..=64 字节、无控制字符），超界显式拒绝。
fn contact_label(display_name: &str) -> Result<String, FfiError> {
    let trimmed = display_name.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    if trimmed.len() > MAX_DISPLAY_NAME_BYTES {
        return Err(store_error(
            ErrorCode::InvalidTarget,
            format!("contact label exceeds {MAX_DISPLAY_NAME_BYTES} bytes"),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(store_error(
            ErrorCode::InvalidTarget,
            "contact label contains control characters",
        ));
    }
    Ok(trimmed.to_owned())
}

/// 无昵称时的展示/落库回退：账户 ID 缩写 `acct:1:<前 8 位 hex>`（14 字节，符合显示名边界）。
///
/// 这是「还没有昵称」的诚实占位（与 UI 展示账户 ID 缩写的回退一致），
/// 不是对端自述或伪造的昵称。
fn short_account_label(account_id: AccountId) -> String {
    let full = account_id.encode();
    let payload = full.strip_prefix("acct:1:").unwrap_or(&full);
    let short: String = payload.chars().take(8).collect();
    format!("acct:1:{short}")
}

/// 小写十六进制编码（种子交给 core 时使用）。
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    text
}

/// 生成账户/设备种子（系统安全随机源）。
fn fresh_seeds() -> ([u8; KEY_SEED_BYTES], [u8; KEY_SEED_BYTES]) {
    let mut account_seed = [0u8; KEY_SEED_BYTES];
    let mut device_seed = [0u8; KEY_SEED_BYTES];
    rand::fill(&mut account_seed);
    rand::fill(&mut device_seed);
    (account_seed, device_seed)
}

/// 账户投影：由账户密钥与设备 ID 派生（ID/指纹/显示名全部来自真实密钥）。
fn account_view_of(account: &LocalAccount, device_id: DeviceId) -> AccountView {
    // 本切片只有初始设备一台，不声称多设备注册表（IDENTITY-011 未实现）。
    let projected = account.view(1);
    AccountView {
        account_id: projected.account_id().encode(),
        device_id: device_id.encode(),
        display_name: projected.display_name().to_owned(),
        fingerprint: projected.fingerprint_text(),
    }
}

/// 首次打开持久库：生成种子并落库（没有昵称输入，显示名用账户 ID 缩写）。
fn save_fresh_account(core: &mut ConversationCore, clock: Clock) -> Result<(), FfiError> {
    let now_ms = clock();
    let (mut account_seed, mut device_seed) = fresh_seeds();
    let account_id = AccountKeys::from_seed(&account_seed).account_id();
    let input = IdentityAccountInput {
        display_name: short_account_label(account_id),
        account_seed_hex: hex_lower(&account_seed),
        device_seed_hex: hex_lower(&device_seed),
        created_at_ms: now_ms,
    };
    // 种子字节用完即清零；十六进制串交给 core（其解码缓冲由 core 用 Zeroizing 清理）。
    account_seed.fill(0);
    device_seed.fill(0);
    core.save_identity_account(input)
        .map_err(|error| core_failure!(error))?;
    Ok(())
}

/// 把库里的联系人行恢复成本地状态机记录（绝不把未知状态静默归一）。
fn restore_contact(view: CoreContactView) -> Result<ContactRecord, FfiError> {
    let peer = AccountId::parse(&view.account_id).map_err(|error| {
        state_error(
            "STATE_CORRUPTED",
            &format!("stored contact has an invalid account id: {error}"),
        )
    })?;
    let fingerprint = Fingerprint::parse(&view.fingerprint).map_err(|error| {
        state_error(
            "STATE_CORRUPTED",
            &format!("stored contact has an invalid fingerprint: {error}"),
        )
    })?;
    let mut contact = Contact::requested(peer, fingerprint);
    let mut restored_state = None;
    match view.state.as_str() {
        "requested" => {}
        "accepted" => contact = Contact::trusted(peer, fingerprint),
        "declined" => contact.decline().map_err(FfiError::from)?,
        "blocked" => contact.block().map_err(FfiError::from)?,
        // identity 的 Contact 没有「直接构造 identity_changed」的入口：用 requested
        // 承载（同样禁止发送），并保留状态供 UI 显示与显式重新确认。
        "identity_changed" => restored_state = Some(ContactState::IdentityChanged),
        other => {
            return Err(state_error(
                "STATE_CORRUPTED",
                &format!("unknown stored contact state {other}"),
            ));
        }
    }
    // 库里的 display_name 是本地标注：等于账户 ID 缩写时还原成「未知」。
    let display_name = if view.display_name == short_account_label(peer) {
        String::new()
    } else {
        view.display_name
    };
    Ok(ContactRecord {
        contact,
        display_name,
        restored_state,
    })
}

/// 落库用的显示名：本地备注为空（未知昵称）时用账户 ID 缩写，避免伪造昵称。
fn persisted_label(record: &ContactRecord) -> String {
    if record.display_name.is_empty() {
        short_account_label(record.contact.peer())
    } else {
        record.display_name.clone()
    }
}

/// 把联系人记录投影成 core 的写入请求（状态/指纹/标注与内存记录一致）。
fn upsert_request(record: &ContactRecord, at_ms: u64) -> Result<ContactUpsertRequest, FfiError> {
    let fingerprint = record
        .contact
        .pinned_fingerprint()
        .or_else(|| record.contact.observed_fingerprint())
        .ok_or_else(|| {
            state_error(
                "STATE_CORRUPTED",
                "contact has no observed identity fingerprint",
            )
        })?;
    Ok(ContactUpsertRequest {
        account_id: record.contact.peer().encode(),
        display_name: persisted_label(record),
        fingerprint: fingerprint.grouped(),
        state: record.effective_state().as_str().to_owned(),
        updated_at_ms: at_ms,
    })
}

/// 把本地联系人记录投影成视图。
fn contact_view(record: &ContactRecord) -> ContactView {
    let contact = &record.contact;
    ContactView {
        // 邀请口令没有独立的请求 ID，联系人的稳定句柄就是对端账户的规范文本。
        contact_id: contact.peer().encode(),
        account_id: contact.peer().encode(),
        display_name: record.display_name.clone(),
        // 优先展示已固定的信任锚；尚未确认时展示收到的观测指纹。
        fingerprint: contact
            .pinned_fingerprint()
            .or_else(|| contact.observed_fingerprint())
            .map(|fingerprint| fingerprint.grouped())
            .unwrap_or_default(),
        state: record.effective_state().as_str().to_owned(),
        can_send: contact.can_send(),
    }
}

#[uniffi::export]
impl IdentityFfi {
    /// 建立**进程内**身份（不写数据库）：长期账户密钥在本进程内生成并绑定一个协调节点。
    ///
    /// - `display_name`：1..=64 字节、非空白、无控制字符；
    /// - `node`：PROTOCOL-001 规范文本 `node:1:<64 位小写 hex>`；
    /// - 失败一律 `FfiError.Store`：`INVALID_TARGET`（显示名）/ `INVALID_ID`（节点）。
    /// - 需要重启后仍在的身份请用 `open_persisted`。
    #[uniffi::constructor]
    pub fn create(display_name: String, node: String) -> Result<Arc<Self>, FfiError> {
        Self::create_with_clock(display_name, node, system_now_ms)
    }

    /// 打开正式加密存储并恢复/建立持久身份。
    ///
    /// - 数据目录必须已存在；`master_key_hex` 是 64 位十六进制（32 字节）；
    /// - 库中已有身份 → 从受保护种子恢复账户与设备（不重新生成）；
    /// - 首次打开 → 生成新账户/设备并落库（显示名用账户 ID 缩写，之后可由上层改名）；
    /// - 同时载入库中全部联系人；
    /// - 单写者：同一目录同一时刻只能有一个句柄，第二个句柄返回 `STORAGE_LOCKED`；
    ///   错误主密钥返回 `STATE_CORRUPTED`（绝不重建空库），畸形密钥返回 `INVALID_INPUT`。
    #[uniffi::constructor]
    pub fn open_persisted(
        database_path: String,
        master_key_hex: String,
        node: String,
    ) -> Result<Arc<Self>, FfiError> {
        Self::open_persisted_with_clock(database_path, master_key_hex, node, system_now_ms)
    }

    /// 只**绑定**恢复目标库（不建身份），用于新机器导入恢复文件。
    ///
    /// 语义：打开（必要时创建）正式库、确认库内没有身份、随即关闭并保持"关闭态"。
    /// 之后调用 [`export_recovery_bundle`] 会以 `CORE_CLOSED` 拒绝，
    /// 而 [`import_recovery_bundle`] 可以在这个空库上执行；导入成功后用
    /// `open_persisted` 打开同一库即可拿到恢复的身份。
    ///
    /// 目标库已有身份 → `RECOVERY_TARGET_NOT_EMPTY`（不覆盖；请改用 `open_persisted` 直接打开）。
    #[uniffi::constructor]
    pub fn open_recovery_target(
        database_path: String,
        master_key_hex: String,
        node: String,
    ) -> Result<Arc<Self>, FfiError> {
        let node = parse_node(&node)?;
        // 校验主密钥形态（失败即 INVALID_INPUT），并确认库内没有身份。
        let _ = decode_master_key_hex(&master_key_hex)?;
        {
            let store = open_formal_store(&database_path, &master_key_hex)?;
            if store
                .load_identity_account()
                .map_err(|error| {
                    let detail = error.to_string();
                    state_error(error.code(), &detail)
                })?
                .is_some()
            {
                return Err(state_error(
                    "RECOVERY_TARGET_NOT_EMPTY",
                    "the target store already holds an identity",
                ));
            }
        }
        // 关闭态占位身份：只为了让对象可构造；@account()` 在导入+重开之前不可信。
        let now_ms = system_now_ms();
        let account = LocalAccount::create("恢复目标", now_ms).map_err(FfiError::from)?;
        let device = DeviceKeys::generate();
        let device_id = device.device_id(account.account_id());
        let view = account_view_of(&account, device_id);
        let mut state = IdentityState {
            account: None,
            device: None,
            account_id: account.account_id(),
            node,
            view,
            contacts: Vec::new(),
            core: None,
            database: Some(database_path),
            master_key_hex: Some(master_key_hex),
            clock: system_now_ms,
            closed: true,
        };
        state.closed = true;
        Ok(Arc::new(Self {
            state: Mutex::new(state),
        }))
    }

    /// IDENTITY-011：设备列表（默认隐藏已撤销设备）。
    ///
    /// 首次调用会把**本机设备**补登记进集合（幂等），因此新建身份后列表里一定有本机一台。
    pub fn list_devices(&self, include_revoked: bool) -> Result<Vec<DeviceViewFfi>, FfiError> {
        self.with_store(|store, local| {
            // 懒登记本机设备（幂等；已撤销不会因此复活）。
            if let Some(public_key) = local.public_key {
                store
                    .upsert_identity_device(&chat_storage::formal::DeviceRegistrationInput {
                        device_id: local.device_id.clone(),
                        account_id: local.account_id.clone(),
                        device_public_key: public_key,
                        label: local.display_name.clone(),
                    })
                    .map_err(storage_failure)?;
            }
            let devices = chat_identity::devices::registered_devices(
                store,
                &local.account_id,
                include_revoked,
            )
            .map_err(device_failure)?;
            Ok(devices
                .into_iter()
                .map(|device| DeviceViewFfi {
                    is_local: device.device_id == local.device_id,
                    device_id: device.device_id,
                    account_id: device.account_id,
                    label: device.label,
                    device_public_key_hex: device.device_public_key,
                    added_at_ms: device.added_at_ms,
                    revoked_at_ms: device.revoked_at_ms,
                    revoked_reason: device.revoked_reason,
                })
                .collect())
        })
    }

    /// IDENTITY-012：撤销一台设备（幂等；**不能撤销本机** → `DEVICE_SELF_REVOKE`）。
    pub fn revoke_device(&self, device_id: String, reason: String) -> Result<bool, FfiError> {
        self.with_store(|store, local| {
            let now_ms = i64::try_from(local.now_ms).map_err(|_| {
                state_error("INVALID_INPUT", "the local clock is out of range")
            })?;
            chat_identity::devices::revoke_registered_device(
                store,
                &local.account_id,
                &device_id,
                &local.device_id,
                &reason,
                now_ms,
            )
            .map_err(device_failure)
        })
    }

    /// 设备是否已撤销（未登记视为未撤销）。
    pub fn device_is_revoked(&self, device_id: String) -> Result<bool, FfiError> {
        self.with_store(|store, _local| {
            chat_identity::devices::device_is_revoked(store, &device_id).map_err(device_failure)
        })
    }

    /// IDENTITY-013：导出身份恢复文件（加密，**不含存储主密钥**）。
    ///
    /// 需要**持久化模式**（`open_persisted`）且库里已有身份，否则 `INVALID_INPUT`。
    /// 导出期间会短暂释放正式库的独占写锁（导出后自动重开并恢复身份句柄）。
    pub fn export_recovery_bundle(&self, passphrase: String) -> Result<Vec<u8>, FfiError> {
        let mut state = self.lock()?;
        state.require_open()?;
        let Some(database) = state.database.clone() else {
            return Err(state_error(
                "INVALID_INPUT",
                "identity is not persisted: open it with open_persisted first",
            ));
        };
        let Some(master_key_hex) = state.master_key_hex.clone() else {
            return Err(state_error(
                "INVALID_INPUT",
                "identity is not persisted: open it with open_persisted first",
            ));
        };
        // 释放独占写锁：导出只需要读，导出后立刻把身份句柄装回去。
        if let Some(core) = state.core.take() {
            core.close();
        }
        let bundle = {
            let store = open_formal_store(&database, &master_key_hex)?;
            chat_identity::recovery::export_recovery_bundle(&store, &passphrase)
                .map_err(recovery_failure)?
        };
        restore_persisted(&mut state, &database, &master_key_hex)?;
        Ok(bundle)
    }

    /// IDENTITY-014：把恢复文件导入本对象的库（**只允许空库**）。
    ///
    /// - 对象必须已 `shutdown()`（先释放独占写锁）；否则显式 `INVALID_STATE_TRANSITION`；
    /// - 目标库已有身份 → `RECOVERY_TARGET_NOT_EMPTY`（绝不覆盖）；
    /// - 导入成功后对象仍是关闭状态：调用方重新 `open_persisted` 即可拿到恢复的身份。
    pub fn import_recovery_bundle(
        &self,
        passphrase: String,
        bundle: Vec<u8>,
    ) -> Result<RecoveryImportViewFfi, FfiError> {
        let state = self.lock()?;
        let Some(database) = state.database.clone() else {
            return Err(state_error(
                "INVALID_INPUT",
                "identity is not persisted: open it with open_persisted first",
            ));
        };
        let Some(master_key_hex) = state.master_key_hex.clone() else {
            return Err(state_error(
                "INVALID_INPUT",
                "identity is not persisted: open it with open_persisted first",
            ));
        };
        if !state.closed {
            return Err(state_error(
                "INVALID_STATE_TRANSITION",
                "close the identity object before importing into its database",
            ));
        }
        let mut store = open_formal_store(&database, &master_key_hex)?;
        let view = chat_identity::recovery::import_recovery_bundle(&mut store, &bundle, &passphrase)
            .map_err(recovery_failure)?;
        Ok(RecoveryImportViewFfi {
            account_id: view.account_id,
            device_id: view.device_id,
            account_public_key: view.account_public_key,
            device_public_key: view.device_public_key,
            display_name: view.display_name,
            created_at_ms: view.created_at_ms,
            devices_digest: view.devices_digest,
        })
    }

    /// 本机账户的脱敏视图。账户在构造时就已建立，因此不会失败。
    pub fn account(&self) -> AccountView {
        self.read().view.clone()
    }

    /// 生成本账户 + 本设备 + 本节点的一次性联系人口令。
    ///
    /// `ttl_ms` 必须在 60 s ..= 7 d（identity 的冻结边界），越界返回 `INVALID_TARGET`。
    /// 返回的 `invite_id`/`expires_at_ms` 等字段来自**刚生成口令的验签回读**，
    /// 不是本地推算值。
    pub fn create_invite(&self, ttl_ms: u64) -> Result<InviteView, FfiError> {
        let state = self.lock()?;
        state.require_open()?;
        let (account, device) = state.keys()?;
        let now_ms = (state.clock)();
        let node = state.node;
        let token = account
            .create_contact_invite(device, node, InviteScope::ContactRequest, now_ms, ttl_ms)
            .map_err(FfiError::from)?;
        // 生成即回读验签：视图里的账户/设备/到期时间/邀请 ID 只能来自已验证的正文。
        let verified = chat_identity::verify_invite(
            token.as_str(),
            &InviteExpectations::at(now_ms).with_node(node),
        )
        .map_err(FfiError::from)?;
        Ok(InviteView {
            token: token.into_string(),
            invite_id: verified.invite_id_hex(),
            expires_at_ms: verified.expires_at_ms(),
            account_id: verified.account_id().encode(),
            device_id: verified.device_id().encode(),
            fingerprint: verified.account_fingerprint().grouped(),
        })
    }

    /// 校验并展示口令绑定的身份（不做任何信任或持久化）。
    ///
    /// 篡改 → `INVALID_SIGNATURE`；过期 → `CURSOR_EXPIRED`；结构畸形/超长 →
    /// `MALFORMED_FRAME` / `CONTENT_TOO_LARGE`；自己的口令 → `INVALID_TARGET`。
    /// 节点只作为提示展示：跨节点口令在这里仍可预览（`accept_invite` 才拒绝）。
    pub fn preview_invite(&self, token: String) -> Result<InviteSummary, FfiError> {
        let state = self.lock()?;
        state.require_open()?;
        let now_ms = (state.clock)();
        let verified = chat_identity::verify_invite(&token, &InviteExpectations::at(now_ms))
            .map_err(FfiError::from)?;
        if verified.account_id() == state.account_id {
            return Err(store_error(
                ErrorCode::InvalidTarget,
                "invite is bound to the local account",
            ));
        }
        Ok(InviteSummary {
            invite_id: verified.invite_id_hex(),
            account_id: verified.account_id().encode(),
            device_id: verified.device_id().encode(),
            fingerprint: verified.account_fingerprint().grouped(),
            node: verified.node().encode(),
            expires_at_ms: verified.expires_at_ms(),
        })
    }

    /// 接受口令：建立**待确认**联系人（`state = "requested"`，`can_send = false`）。
    ///
    /// - 同一账户重复粘贴返回同一条记录，不产生重复申请；
    /// - `display_name` 是本地备注（口令不携带对方昵称）：空串表示未知、保持已有备注；
    /// - 自己的口令、以及节点与本对象不一致的口令返回 `INVALID_TARGET`；
    /// - 已存在的联系人不会被静默覆盖信任锚：指纹变化通过 `identity_changed` 状态报告；
    /// - 持久对象先在 core 落库、成功后才提交内存（以持久化为准）。
    pub fn accept_invite(
        &self,
        token: String,
        display_name: String,
    ) -> Result<ContactView, FfiError> {
        let mut state = self.lock()?;
        state.require_open()?;
        let now_ms = (state.clock)();
        let label = contact_label(&display_name)?;
        let node = state.node;
        let account_id = state.account_id;
        let verified =
            chat_identity::verify_invite(&token, &InviteExpectations::at(now_ms).with_node(node))
                .map_err(FfiError::from)?;
        let peer = verified.account_id();
        if peer == account_id {
            return Err(store_error(
                ErrorCode::InvalidTarget,
                "invite is bound to the local account",
            ));
        }
        let observed = verified.account_fingerprint();
        let existing = state
            .contacts
            .iter()
            .position(|record| record.contact.peer() == peer);
        // 先在副本上推进状态机，落库成功后才提交到内存。
        let mut updated = match existing {
            Some(index) => state.contacts[index].clone(),
            None => ContactRecord::fresh(Contact::requested(peer, observed), String::new()),
        };
        if !label.is_empty() {
            updated.display_name = label;
        }
        // 既有联系人再观测到新指纹时由状态机转 identity_changed（不静默覆盖信任锚）。
        updated
            .contact
            .record_identity(observed)
            .map_err(FfiError::from)?;
        let request = upsert_request(&updated, now_ms)?;
        if let Some(core) = state.core.as_mut() {
            core.upsert_contact(request)
                .map_err(|error| core_failure!(error))?;
        }
        match existing {
            Some(index) => state.contacts[index] = updated,
            None => state.contacts.push(updated),
        }
        let record = match existing {
            Some(index) => &state.contacts[index],
            None => state
                .contacts
                .last()
                .ok_or_else(|| state_error("STATE_CORRUPTED", "accepted contact is missing"))?,
        };
        Ok(contact_view(record))
    }

    /// 当前联系人列表（按首次加入顺序；关闭后返回关闭前快照）。
    pub fn contacts(&self) -> Vec<ContactView> {
        self.read().contacts.iter().map(contact_view).collect()
    }

    /// 用户重新核对新指纹后确认身份变化（唯一能从 `identity_changed` 回到 `accepted`
    /// 的入口）。
    ///
    /// `contact_id` 必须原样使用 `ContactView.contact_id`。未知/畸形句柄返回
    /// `INVALID_TARGET`；当前状态不是 `identity_changed` 时返回
    /// `INVALID_STATE_TRANSITION`（普通确认不能绕过身份变化）。
    /// 持久对象同样先落库（`accepted`）再提交内存。
    pub fn confirm_identity_change(&self, contact_id: String) -> Result<ContactView, FfiError> {
        let mut state = self.lock()?;
        state.require_open()?;
        let now_ms = (state.clock)();
        let peer = AccountId::parse(&contact_id).map_err(|error| {
            store_error(
                ErrorCode::InvalidTarget,
                format!("unknown contact id: {error}"),
            )
        })?;
        let index = state
            .contacts
            .iter()
            .position(|record| record.contact.peer() == peer)
            .ok_or_else(|| {
                store_error(
                    ErrorCode::InvalidTarget,
                    "unknown contact id: no local record",
                )
            })?;
        let mut updated = state.contacts[index].clone();
        if updated.restored_state == Some(ContactState::IdentityChanged) {
            // 从库中恢复的 identity_changed：用户已核对新指纹 → 用观测指纹重新固定信任锚
            // （与 identity 的 confirm_identity_change 同义，因为旧信任锚不在联系人行里）。
            let observed = updated.contact.observed_fingerprint().ok_or_else(|| {
                store_error(
                    ErrorCode::InvalidCredential,
                    "contact has no observed identity fingerprint",
                )
            })?;
            updated.contact = Contact::trusted(peer, observed);
            updated.restored_state = None;
        } else {
            updated
                .contact
                .confirm_identity_change()
                .map_err(FfiError::from)?;
        }
        let request = upsert_request(&updated, now_ms)?;
        if let Some(core) = state.core.as_mut() {
            core.upsert_contact(request)
                .map_err(|error| core_failure!(error))?;
        }
        state.contacts[index] = updated;
        Ok(contact_view(&state.contacts[index]))
    }

    /// 释放账户/设备密钥并关闭正式存储句柄；重复调用幂等。
    ///
    /// 方法名用 `shutdown` 而不是 `close`：UniFFI 会为每个对象生成
    /// `AutoCloseable.close()`，同名会与生成代码产生 JVM 签名冲突。
    /// 关闭后所有需要密钥的方法返回 `CORE_CLOSED`；`account()`/`contacts()` 仍返回
    /// 关闭前的公开快照（它们的签名不能返回错误）。
    pub fn shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 冻结的“现在”：2023-11-14T22:13:20Z（与 identity 的测试向量同一时刻）。
    const NOW_MS: u64 = 1_700_000_000_000;
    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
    const MINUTE_MS: u64 = 60_000;
    const MAX_TTL_MS: u64 = 7 * DAY_MS;
    const NODE_HEX: &str = "3333333333333333333333333333333333333333333333333333333333333333";
    const MASTER_KEY_HEX: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const WRONG_MASTER_KEY_HEX: &str =
        "ff112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn node_text() -> String {
        format!("node:1:{NODE_HEX}")
    }

    fn fixed_clock() -> u64 {
        NOW_MS
    }

    /// 只有在 expiry 测试里推进的时钟（各测试用自己的静态量，避免并行互相干扰）。
    static EXPIRY_CLOCK_MS: AtomicU64 = AtomicU64::new(NOW_MS);

    fn expiry_clock() -> u64 {
        EXPIRY_CLOCK_MS.load(Ordering::Relaxed)
    }

    fn new_identity(display_name: &str) -> Arc<IdentityFfi> {
        IdentityFfi::create_with_clock(display_name.to_owned(), node_text(), fixed_clock)
            .expect("create identity")
    }

    fn expect_code<T>(result: Result<T, FfiError>, expected: &str) {
        match result {
            Ok(_) => panic!("expected {expected}, got a successful result"),
            Err(FfiError::Store { code, reason }) => {
                assert_eq!(code, expected, "reason: {reason}");
            }
        }
    }

    /// 临时数据目录（每个测试独立，Drop 时删除）。
    struct Temp {
        dir: PathBuf,
    }

    impl Temp {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "chat-ffi-identity-{tag}-{}-{nanos}-{counter}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp dir");
            Self { dir }
        }

        fn database(&self) -> String {
            self.dir.join("chat.db").to_string_lossy().into_owned()
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// 打开（首次会创建）持久身份。
    fn open_persisted_at(dir: &Temp) -> Arc<IdentityFfi> {
        IdentityFfi::open_persisted_with_clock(
            dir.database(),
            MASTER_KEY_HEX.to_owned(),
            node_text(),
            fixed_clock,
        )
        .expect("open persisted identity")
    }

    /// 与 core 测试相同的种子十六进制生成方式。
    fn seed_hex(start: u8) -> String {
        let mut text = String::with_capacity(64);
        for offset in 0..32u8 {
            text.push_str(&format!("{:02x}", start.wrapping_add(offset)));
        }
        text
    }

    #[test]
    fn account_and_invite_round_trip_through_the_ffi_shape() {
        let alice = new_identity("Alice");
        let account = alice.account();
        assert!(account.account_id.starts_with("acct:1:"));
        assert!(account.device_id.starts_with("dev:1:"));
        assert_eq!(account.display_name, "Alice");
        assert_eq!(account.fingerprint.len(), 79);
        assert_eq!(account.fingerprint.split(' ').count(), 16);
        assert!(alice.contacts().is_empty());

        let invite = alice.create_invite(DAY_MS).expect("create invite");
        assert!(invite.token.starts_with("inv1."));
        assert_eq!(invite.invite_id.len(), 32);
        assert!(invite.invite_id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(invite.expires_at_ms, NOW_MS + DAY_MS);
        assert_eq!(invite.account_id, account.account_id);
        assert_eq!(invite.device_id, account.device_id);
        assert_eq!(invite.fingerprint, account.fingerprint);

        // 对端 preview 与生成结果来自同一段签名正文。
        let bob = new_identity("Bob");
        let summary = bob.preview_invite(invite.token.clone()).expect("preview");
        assert_eq!(summary.invite_id, invite.invite_id);
        assert_eq!(summary.account_id, invite.account_id);
        assert_eq!(summary.device_id, invite.device_id);
        assert_eq!(summary.fingerprint, invite.fingerprint);
        assert_eq!(summary.node, node_text());
        assert_eq!(summary.expires_at_ms, invite.expires_at_ms);
        assert!(
            bob.contacts().is_empty(),
            "preview must not create a contact"
        );
    }

    #[test]
    fn projection_matches_the_frozen_identity_vectors() {
        // 账户种子 0x01 / 设备种子 0x11 的冻结向量（identity 用 OpenSSL/Node 独立算出）。
        let identity = IdentityFfi::from_seeds("Vector", &node_text(), 0x01, 0x11, fixed_clock)
            .expect("identity from seeds");
        let account = identity.account();
        assert_eq!(
            account.account_id,
            "acct:1:804d17d85c433dc00878aceda0dff5d4d6d72dd6dbdabda188e4ae177304bffa"
        );
        assert_eq!(
            account.device_id,
            "dev:1:a4ec09edbb8ceb00f41b77fed48dbda0796e4456e2a99e813dfcad390bc9e1f5"
        );
        assert_eq!(
            account.fingerprint,
            "7EC8 C5E1 A37C C538 AFCB A0B0 BD28 C9E1 5A4B 1A3E 53DC 0F18 BE09 6DF9 4742 8EB4"
        );
    }

    #[test]
    fn production_constructor_uses_the_system_clock() {
        let alice = IdentityFfi::create("Alice".to_owned(), node_text()).expect("create");
        let bob = IdentityFfi::create("Bob".to_owned(), node_text()).expect("create");
        assert_eq!(alice.account().display_name, "Alice");
        let invite = alice.create_invite(DAY_MS).expect("create invite");
        // 系统时钟在 2020 之后，且到期时间由真实签发时间加上 ttl 得到。
        assert!(invite.expires_at_ms > 1_700_000_000_000);
        let summary = bob.preview_invite(invite.token.clone()).expect("preview");
        assert_eq!(summary.invite_id, invite.invite_id);
        assert_eq!(summary.expires_at_ms, invite.expires_at_ms);
        let contact = bob
            .accept_invite(invite.token, String::new())
            .expect("accept");
        assert_eq!(contact.state, "requested");
        assert!(!contact.can_send);
    }

    #[test]
    fn tampered_tokens_are_rejected_with_invalid_signature() {
        let alice = new_identity("Alice");
        let bob = new_identity("Bob");
        let invite = alice.create_invite(DAY_MS).expect("create invite");
        let mut tampered = invite.token.clone();
        let last = tampered.pop().expect("token last char");
        tampered.push(if last == '0' { '1' } else { '0' });
        expect_code(
            bob.preview_invite(tampered.clone()),
            ErrorCode::InvalidSignature.as_str(),
        );
        expect_code(
            bob.accept_invite(tampered, String::new()),
            ErrorCode::InvalidSignature.as_str(),
        );
        assert!(bob.contacts().is_empty(), "验证失败不得产生联系人");
    }

    #[test]
    fn expired_invites_are_rejected_with_cursor_expired() {
        let alice = IdentityFfi::create_with_clock("Alice".to_owned(), node_text(), expiry_clock)
            .expect("create identity");
        let bob = IdentityFfi::create_with_clock("Bob".to_owned(), node_text(), expiry_clock)
            .expect("create identity");
        let invite = alice.create_invite(MINUTE_MS).expect("create invite");
        assert_eq!(invite.expires_at_ms, NOW_MS + MINUTE_MS);
        // 到期时刻即过期（identity 冻结语义）。
        EXPIRY_CLOCK_MS.store(NOW_MS + MINUTE_MS, Ordering::Relaxed);
        expect_code(
            bob.preview_invite(invite.token.clone()),
            ErrorCode::CursorExpired.as_str(),
        );
        expect_code(
            bob.accept_invite(invite.token, String::new()),
            ErrorCode::CursorExpired.as_str(),
        );
        assert!(bob.contacts().is_empty());
        EXPIRY_CLOCK_MS.store(NOW_MS, Ordering::Relaxed);
    }

    #[test]
    fn self_invites_and_foreign_nodes_are_rejected_with_invalid_target() {
        let alice = new_identity("Alice");
        let own = alice.create_invite(DAY_MS).expect("create invite");
        expect_code(
            alice.preview_invite(own.token.clone()),
            ErrorCode::InvalidTarget.as_str(),
        );
        expect_code(
            alice.accept_invite(own.token, String::new()),
            ErrorCode::InvalidTarget.as_str(),
        );

        // 跨节点口令：preview 只把节点当提示展示，accept 拒绝。
        let foreign_node = format!("node:1:{}", "44".repeat(32));
        let elsewhere =
            IdentityFfi::create_with_clock("Carol".to_owned(), foreign_node.clone(), fixed_clock)
                .expect("create identity");
        let foreign = elsewhere.create_invite(DAY_MS).expect("create invite");
        let summary = alice
            .preview_invite(foreign.token.clone())
            .expect("preview");
        assert_eq!(summary.node, foreign_node);
        expect_code(
            alice.accept_invite(foreign.token, String::new()),
            ErrorCode::InvalidTarget.as_str(),
        );
        assert!(alice.contacts().is_empty());
    }

    #[test]
    fn accept_invite_creates_a_requested_contact_only() {
        let alice = new_identity("Alice");
        let bob = new_identity("Bob");
        let invite = alice.create_invite(DAY_MS).expect("create invite");
        let contact = bob
            .accept_invite(invite.token.clone(), "Alice（备注）".to_owned())
            .expect("accept invite");
        assert_eq!(contact.contact_id, alice.account().account_id);
        assert_eq!(contact.account_id, contact.contact_id);
        assert_eq!(contact.display_name, "Alice（备注）");
        assert_eq!(contact.fingerprint, alice.account().fingerprint);
        assert_eq!(contact.state, "requested");
        assert!(!contact.can_send, "未确认的联系人禁止发送");
        assert_eq!(bob.contacts(), vec![contact.clone()]);

        // 同一口令重复粘贴：不产生重复申请，也不丢本地备注。
        let again = bob
            .accept_invite(invite.token, String::new())
            .expect("idempotent accept");
        assert_eq!(again, contact);
        assert_eq!(bob.contacts().len(), 1);
        assert!(!again.can_send);
    }

    #[test]
    fn confirm_identity_change_rejects_illegal_calls() {
        let alice = new_identity("Alice");
        let bob = new_identity("Bob");
        let invite = alice.create_invite(DAY_MS).expect("create invite");
        let contact = bob
            .accept_invite(invite.token, String::new())
            .expect("accept invite");
        // requested 状态不能靠 confirm_identity_change 直接变 accepted。
        expect_code(
            bob.confirm_identity_change(contact.contact_id.clone()),
            ErrorCode::InvalidStateTransition.as_str(),
        );
        // 未知账户。
        expect_code(
            bob.confirm_identity_change(format!("acct:1:{}", "00".repeat(32))),
            ErrorCode::InvalidTarget.as_str(),
        );
        // 畸形句柄。
        expect_code(
            bob.confirm_identity_change("not-an-account-id".to_owned()),
            ErrorCode::InvalidTarget.as_str(),
        );
        assert_eq!(bob.contacts(), vec![contact]);
    }

    #[test]
    fn display_name_node_and_lifetime_bounds_are_enforced() {
        expect_code(
            IdentityFfi::create(String::new(), node_text()),
            ErrorCode::InvalidTarget.as_str(),
        );
        expect_code(
            IdentityFfi::create("a".repeat(MAX_DISPLAY_NAME_BYTES + 1), node_text()),
            ErrorCode::InvalidTarget.as_str(),
        );
        expect_code(
            IdentityFfi::create("a\nb".to_owned(), node_text()),
            ErrorCode::InvalidTarget.as_str(),
        );
        expect_code(
            IdentityFfi::create("Alice".to_owned(), String::new()),
            ErrorCode::InvalidId.as_str(),
        );
        expect_code(
            IdentityFfi::create("Alice".to_owned(), "demo-node".to_owned()),
            ErrorCode::InvalidId.as_str(),
        );

        let alice = new_identity("Alice");
        for ttl in [0, 1, MINUTE_MS - 1, MAX_TTL_MS + 1, u64::MAX] {
            expect_code(alice.create_invite(ttl), ErrorCode::InvalidTarget.as_str());
        }
        assert!(alice.create_invite(MINUTE_MS).is_ok());
        assert!(alice.create_invite(MAX_TTL_MS).is_ok());

        // 过长/畸形口令一律有界拒绝。
        let oversized = format!("inv1.{}", "a".repeat(2048));
        expect_code(
            alice.preview_invite(oversized),
            ErrorCode::ContentTooLarge.as_str(),
        );
        expect_code(
            alice.preview_invite("inv2.00.11".to_owned()),
            ErrorCode::ProtocolUnsupported.as_str(),
        );
        expect_code(
            alice.preview_invite("inv1.aabb.cc".to_owned()),
            ErrorCode::MalformedFrame.as_str(),
        );
    }

    #[test]
    fn shutdown_releases_keys_and_rejects_further_key_work() {
        let alice = new_identity("Alice");
        let before = alice.account();
        let invite = alice.create_invite(DAY_MS).expect("create invite");
        alice.shutdown();
        alice.shutdown(); // 幂等

        for error in [
            alice.create_invite(DAY_MS).unwrap_err(),
            alice.preview_invite(invite.token.clone()).unwrap_err(),
            alice
                .accept_invite(invite.token, String::new())
                .unwrap_err(),
            alice
                .confirm_identity_change(before.account_id.clone())
                .unwrap_err(),
        ] {
            match error {
                FfiError::Store { code, .. } => assert_eq!(code, "CORE_CLOSED"),
            }
        }
        // 公开投影仍可读（这两个方法的签名没有 Result 可返回）。
        assert_eq!(alice.account(), before);
        assert!(alice.contacts().is_empty());
    }

    // ---- 持久身份（open_persisted） ----

    #[test]
    fn persisted_identity_survives_reopen_with_the_same_account() {
        let temp = Temp::new("reopen");
        let first = open_persisted_at(&temp);
        let account = first.account();
        // 首次打开没有昵称输入：落库的显示名是账户 ID 缩写（诚实占位，不是伪造昵称）。
        let account_id = AccountId::parse(&account.account_id).expect("account id");
        assert_eq!(account.display_name, short_account_label(account_id));
        assert!(account.account_id.starts_with("acct:1:"));
        assert!(account.device_id.starts_with("dev:1:"));
        assert_eq!(account.fingerprint.len(), 79);
        assert!(first.contacts().is_empty());

        // 同一目录 + 同一主密钥重开：ID/设备/指纹/显示名完全一致（没有重新生成账户）。
        first.shutdown();
        let second = open_persisted_at(&temp);
        assert_eq!(second.account(), account);
        assert!(second.contacts().is_empty());

        // 身份确实在库里（不是内存假象）：直接用 core 读回投影与种子。
        second.shutdown();
        let core = ConversationCore::open(&temp.database(), MASTER_KEY_HEX).expect("open core");
        let stored = core
            .load_identity_account()
            .expect("load projection")
            .expect("persisted account is present");
        assert_eq!(stored.account_id, account.account_id);
        assert_eq!(stored.device_id, account.device_id);
        assert_eq!(stored.fingerprint, account.fingerprint);
        assert_eq!(stored.display_name, account.display_name);
        assert!(
            core.open_persisted_identity()
                .expect("restore keys")
                .is_some(),
            "受保护种子必须已经落库"
        );
        core.close();
    }

    #[test]
    fn persisted_contacts_survive_reopen_and_keep_their_state() {
        let alice_dir = Temp::new("contacts-alice");
        let bob_dir = Temp::new("contacts-bob");
        let alice = open_persisted_at(&alice_dir);
        let bob = open_persisted_at(&bob_dir);
        let invite = alice.create_invite(DAY_MS).expect("create invite");
        let contact = bob
            .accept_invite(invite.token.clone(), "Alice（备注）".to_owned())
            .expect("accept invite");
        assert_eq!(contact.state, "requested");
        assert!(!contact.can_send);

        // 联系人已落库：重开后仍在，状态/指纹/标注不变。
        bob.shutdown();
        let reopened = open_persisted_at(&bob_dir);
        let listed = reopened.contacts();
        assert_eq!(listed, vec![contact.clone()]);
        assert_eq!(listed[0].contact_id, alice.account().account_id);
        assert_eq!(listed[0].state, "requested");
        assert_eq!(listed[0].fingerprint, alice.account().fingerprint);
        assert_eq!(listed[0].display_name, "Alice（备注）");
        assert!(!listed[0].can_send, "重开后仍不允许发送");

        // 同一口令重复粘贴仍是同一条（落库幂等）。
        let again = reopened
            .accept_invite(invite.token, String::new())
            .expect("idempotent accept");
        assert_eq!(again, contact);
        assert_eq!(reopened.contacts().len(), 1);

        // 未知昵称（空串）落库为账户 ID 缩写，重开后还原成空串（契约稳定）。
        let carol_dir = Temp::new("contacts-carol");
        let carol = open_persisted_at(&carol_dir);
        let unnamed = carol
            .accept_invite(
                alice.create_invite(DAY_MS).expect("create invite").token,
                String::new(),
            )
            .expect("accept without a local label");
        assert_eq!(unnamed.display_name, "", "空串表示未知本地备注");
        carol.shutdown();
        let carol_reopened = open_persisted_at(&carol_dir);
        assert_eq!(carol_reopened.contacts()[0].display_name, "");
        assert_eq!(carol_reopened.contacts()[0].state, "requested");
        assert!(!carol_reopened.contacts()[0].can_send);
    }

    #[test]
    fn wrong_master_key_is_rejected_with_a_stable_code() {
        let temp = Temp::new("wrong-key");
        let identity = open_persisted_at(&temp);
        let account = identity.account();
        identity.shutdown();

        expect_code(
            IdentityFfi::open_persisted_with_clock(
                temp.database(),
                WRONG_MASTER_KEY_HEX.to_owned(),
                node_text(),
                fixed_clock,
            ),
            "STATE_CORRUPTED",
        );
        // 错误密钥不会重建空库：正确密钥仍能读回同一账户。
        let recovered = open_persisted_at(&temp);
        assert_eq!(recovered.account(), account);
    }

    #[test]
    fn persisted_shutdown_closes_the_core_and_rejects_key_work() {
        let temp = Temp::new("persisted-shutdown");
        let identity = open_persisted_at(&temp);
        let before = identity.account();
        let invite = identity.create_invite(DAY_MS).expect("create invite");
        identity.shutdown();
        identity.shutdown(); // 幂等

        // core 真的关闭了：单写者锁已释放，同一目录可以立刻再开一个句柄。
        let reopened = open_persisted_at(&temp);
        assert_eq!(reopened.account(), before);

        for error in [
            identity.create_invite(DAY_MS).unwrap_err(),
            identity.preview_invite(invite.token.clone()).unwrap_err(),
            identity
                .accept_invite(invite.token, String::new())
                .unwrap_err(),
            identity
                .confirm_identity_change(before.account_id.clone())
                .unwrap_err(),
        ] {
            match error {
                FfiError::Store { code, .. } => assert_eq!(code, "CORE_CLOSED"),
            }
        }
        assert_eq!(identity.account(), before);
        assert!(identity.contacts().is_empty());
    }

    #[test]
    fn restored_identity_change_requires_explicit_reconfirmation() {
        let temp = Temp::new("identity-change");
        let peer = AccountId::from_bytes([0x77; 32]);
        let fingerprint = Fingerprint::from_bytes([0x41; 32]);
        // 先用 core 直接写入一个 identity_changed 联系人（模拟后续 core 的入站身份观测）。
        {
            let mut core =
                ConversationCore::open(&temp.database(), MASTER_KEY_HEX).expect("open core");
            core.save_identity_account(IdentityAccountInput {
                display_name: "Alice".to_owned(),
                account_seed_hex: seed_hex(1),
                device_seed_hex: seed_hex(101),
                created_at_ms: NOW_MS,
            })
            .expect("save identity");
            core.upsert_contact(ContactUpsertRequest {
                account_id: peer.encode(),
                display_name: "Bob".to_owned(),
                fingerprint: fingerprint.grouped(),
                state: "identity_changed".to_owned(),
                updated_at_ms: NOW_MS,
            })
            .expect("seed identity_changed contact");
            core.close();
        }

        let identity = open_persisted_at(&temp);
        let listed = identity.contacts();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].display_name, "Bob");
        assert_eq!(listed[0].state, "identity_changed");
        assert!(!listed[0].can_send, "身份变化必须阻断发送");

        // 用户重新核对后确认：状态机把观测指纹重新固定为信任锚，并落库。
        let confirmed = identity
            .confirm_identity_change(listed[0].contact_id.clone())
            .expect("confirm after re-verification");
        assert_eq!(confirmed.state, "accepted");
        assert!(confirmed.can_send);

        identity.shutdown();
        let reopened = open_persisted_at(&temp);
        assert_eq!(reopened.contacts(), vec![confirmed]);
    }
}
