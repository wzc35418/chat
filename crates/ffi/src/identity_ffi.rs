//! 身份层的 UniFFI 对象：Kotlin 侧「口令加好友」竖直切片。
//!
//! 本对象把 [chat_identity] 的账户、邀请口令与联系人状态机投影成 UniFFI 对象，
//! 严格按 UI 约定暴露 `create / account / create_invite / preview_invite /
//! accept_invite / contacts / confirm_identity_change / shutdown` 八个入口。
//!
//! 与既有 `local_store.rs`（正式加密存储）不共享任何状态，但**复用同一个**
//! [`FfiError`]：`code` 直接取 identity 映射出的 PROTOCOL-014 冻结码
//! （INVALID_TARGET / INVALID_SIGNATURE / CURSOR_EXPIRED / INVALID_STATE_TRANSITION /
//! CONTENT_TOO_LARGE …），Kotlin 侧只需要 `catch (e: FfiException.Store)` 一处。
//!
//! 安全边界（与 docs/implementation/identity.md 的 IDENTITY-001/004/005/006/007 对应）：
//!
//! - 口令只在验签 + 期限 + 绑定校验通过后才被解释；本对象不提供「先解析、后验签」
//!   的第二套入口，`preview_invite` 与 `accept_invite` 共用同一条校验链；
//! - 自己签发的口令不能当成联系人：目标账户等于本机账户时显式 INVALID_TARGET；
//! - `accept_invite` 要求口令绑定的节点与本对象创建时传入的节点一致，否则
//!   INVALID_TARGET（见模块文档的「已知限制」：`preview_invite` 只把节点当提示展示）；
//! - 接受邀请只产生 `requested` 联系人（`can_send == false`），绝不自动信任；
//! - `identity_changed` 只能由显式 `confirm_identity_change` 复位，普通接受无法绕过；
//! - 时间由内部时钟提供（生产 = 系统 UTC 毫秒；测试注入固定时钟），本层不做时钟
//!   漂移策略；
//! - 不写数据库、不发网络请求：一次性消费、outbox 与事件流由后续 core 集成补齐，
//!   本层不假装已完成持久化。
//!
//! 线程安全：UniFFI 要求对象可跨线程共享，因此内部状态用 `std::sync::Mutex`
//! 串行化；锁中毒时带返回值的调用显式返回 `CORE_PANICKED`。

use std::sync::{Arc, Mutex, MutexGuard};

use chat_identity::{
    Contact, DeviceKeys, IdentityError, InviteExpectations, InviteScope, LocalAccount,
    MAX_DISPLAY_NAME_BYTES,
};
use chat_protocol::{AccountId, CoordinatorId, ErrorCode};

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
struct ContactRecord {
    contact: Contact,
    display_name: String,
}

/// 对象内部状态。`account`/`device` 持有密钥材料，`shutdown` 后置空。
struct IdentityState {
    account: Option<LocalAccount>,
    device: Option<DeviceKeys>,
    account_id: AccountId,
    node: CoordinatorId,
    /// 公开投影快照（不含密钥，`shutdown` 后仍可读）。
    view: AccountView,
    contacts: Vec<ContactRecord>,
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

    /// 关闭：立即释放账户与设备密钥（`SigningKey` 在 drop 时自行清零）。
    ///
    /// `account()`/`contacts()` 没有 `Result` 可返回，因此公开投影保留为关闭前快照；
    /// 需要密钥的操作一律返回 `CORE_CLOSED`。
    fn close(&mut self) {
        self.closed = true;
        self.account = None;
        self.device = None;
    }
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
        clock: Clock,
    ) -> Self {
        let account_id = account.account_id();
        let device_id = device.device_id(account_id);
        // 本切片只有初始设备一台，不声称多设备注册表（IDENTITY-011 未实现）。
        let projected = account.view(1);
        let view = AccountView {
            account_id: projected.account_id().encode(),
            device_id: device_id.encode(),
            display_name: projected.display_name().to_owned(),
            fingerprint: projected.fingerprint_text(),
        };
        Self {
            state: Mutex::new(IdentityState {
                account: Some(account),
                device: Some(device),
                account_id,
                node,
                view,
                contacts: Vec::new(),
                clock,
                closed: false,
            }),
        }
    }

    /// 时钟注入点（生产传 `system_now_ms`，测试传固定时钟）。
    fn create_with_clock(
        display_name: String,
        node: String,
        clock: Clock,
    ) -> Result<Arc<Self>, FfiError> {
        let node = parse_node(&node)?;
        let now_ms = clock();
        let account = LocalAccount::create(&display_name, now_ms).map_err(FfiError::from)?;
        let device = DeviceKeys::generate();
        Ok(Arc::new(Self::assemble(node, account, device, clock)))
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
        use chat_identity::KEY_SEED_BYTES;
        let node = parse_node(node)?;
        let now_ms = clock();
        let account =
            LocalAccount::from_seed(display_name, now_ms, &[account_seed; KEY_SEED_BYTES])
                .map_err(FfiError::from)?;
        let device = DeviceKeys::from_seed(&[device_seed; KEY_SEED_BYTES]);
        Ok(Arc::new(Self::assemble(node, account, device, clock)))
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
        state: contact.state().as_str().to_owned(),
        can_send: contact.can_send(),
    }
}

#[uniffi::export]
impl IdentityFfi {
    /// 建立本机账户（长期账户密钥在本进程内生成）并绑定一个协调节点。
    ///
    /// - `display_name`：1..=64 字节、非空白、无控制字符；
    /// - `node`：PROTOCOL-001 规范文本 `node:1:<64 位小写 hex>`；
    /// - 失败一律 `FfiError.Store`：`INVALID_TARGET`（显示名）/ `INVALID_ID`（节点）。
    /// - 不写数据库、不做密钥保护：账户只活在本次进程里（见模块文档的已知限制）。
    #[uniffi::constructor]
    pub fn create(display_name: String, node: String) -> Result<Arc<Self>, FfiError> {
        Self::create_with_clock(display_name, node, system_now_ms)
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
    /// - 已存在的联系人不会被静默覆盖信任锚：指纹变化通过 `identity_changed` 状态报告。
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
        let index = match existing {
            Some(index) => index,
            None => {
                let index = state.contacts.len();
                state.contacts.push(ContactRecord {
                    contact: Contact::requested(peer, observed),
                    display_name: String::new(),
                });
                index
            }
        };
        let record = &mut state.contacts[index];
        if !label.is_empty() {
            record.display_name = label;
        }
        // 既有联系人再观测到新指纹时由状态机转 identity_changed（不静默覆盖信任锚）。
        record
            .contact
            .record_identity(observed)
            .map_err(FfiError::from)?;
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
    pub fn confirm_identity_change(&self, contact_id: String) -> Result<ContactView, FfiError> {
        let mut state = self.lock()?;
        state.require_open()?;
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
        let record = &mut state.contacts[index];
        record
            .contact
            .confirm_identity_change()
            .map_err(FfiError::from)?;
        Ok(contact_view(record))
    }

    /// 释放账户/设备密钥；重复调用幂等。
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
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 冻结的“现在”：2023-11-14T22:13:20Z（与 identity 的测试向量同一时刻）。
    const NOW_MS: u64 = 1_700_000_000_000;
    const DAY_MS: u64 = 24 * 60 * 60 * 1_000;
    const MINUTE_MS: u64 = 60_000;
    const MAX_TTL_MS: u64 = 7 * DAY_MS;
    const NODE_HEX: &str = "3333333333333333333333333333333333333333333333333333333333333333";

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
}
