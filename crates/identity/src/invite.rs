//! IDENTITY-004/005：不透明的一次性联系人口令。
//!
//! 文本形式（版本前缀 + 固定字节正文 + 账户签名）：
//!
//! ~~~text
//! inv1.<hex(canonical fixed body)>.<hex(64B signature)>
//! ~~~
//!
//! - 正文是固定宽度二进制布局（见 [BODY_BYTES]），携带账户 ID、账户公钥、
//!   设备 ID、协调节点、作用域、签发/失效时间与 16 字节随机 nonce；
//! - 签名原文 = 目的域 || chat-protocol canonical 编码（位置固定列表）；
//! - 解析先限长、再校验 ASCII/前缀/hex 规范形式/正文长度/版本/时间窗口，
//!   然后**先验签、后看时间**，最后比对调用方期望的节点/账户/设备与信任锚。
//!
//! 口令里不含任何私钥；Debug 只打印前缀与长度。一次性消费状态需要持久化，
//! 由 storage 集成任务负责（本层只做纯校验，见计划书「已知限制」）。

use core::fmt;

use chat_protocol::canonical::{ProtocolVersion, WireValue, encode_canonical};
use chat_protocol::{AccountId, CoordinatorId, DeviceId};

use crate::error::{IdentityError, Result};
use crate::keys::{
    AccountKeys, DeviceKeys, Fingerprint, PUBLIC_KEY_BYTES, SIGNATURE_BYTES, TrustAnchor,
    account_id_from_public_key, device_id_for, verify_signature,
};

/// 口令文本版本前缀。
pub const INVITE_TOKEN_VERSION: &str = "inv1";
/// 口令文本长度上限。
pub const MAX_INVITE_TOKEN_BYTES: usize = 1024;
/// 邀请最短有效期：1 分钟。
pub const MIN_INVITE_TTL_MS: u64 = 60_000;
/// 邀请最长有效期：7 天。
pub const MAX_INVITE_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
/// 产品默认有效期：24 小时（见 docs/design/CONTACT_INVITE_DESKTOP.md）。
pub const DEFAULT_INVITE_TTL_MS: u64 = 24 * 60 * 60 * 1_000;
/// 允许的时钟偏差：60 秒。
pub const INVITE_CLOCK_SKEW_MS: u64 = 60_000;
/// 邀请签名域。
pub const INVITE_SIGNING_DOMAIN: &[u8] = b"chat/p1/contact-invite-signature/v1";
/// 邀请规范 schema 版本。
pub const INVITE_SCHEMA_VERSION: u64 = 1;
/// 邀请 nonce 字节数。
pub const INVITE_NONCE_BYTES: usize = 16;
/// 固定正文长度（字节）。
pub const BODY_BYTES: usize = 1 + 32 + 32 + 32 + 32 + 1 + 8 + 8 + INVITE_NONCE_BYTES;

const BODY_HEX_CHARS: usize = BODY_BYTES * 2;
const SIGNATURE_HEX_CHARS: usize = SIGNATURE_BYTES * 2;

/// 邀请作用域。当前只有 contact-request；其它值必须拒绝。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum InviteScope {
    /// 仅允许发起联系人请求。
    ContactRequest = 1,
}

impl InviteScope {
    /// 冻结数值。
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// 由冻结数值还原。
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(InviteScope::ContactRequest),
            _ => None,
        }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    text
}

fn hex_decode(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return Err(IdentityError::MalformedToken);
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let (pairs, remainder) = bytes.as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for pair in pairs {
        out.push((hex_digit(pair[0])? << 4) | hex_digit(pair[1])?);
    }
    Ok(out)
}

fn hex_digit(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(IdentityError::MalformedToken),
    }
}

/// 口令正文（解析后）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InviteBody {
    account_id: AccountId,
    account_public_key: [u8; PUBLIC_KEY_BYTES],
    device_id: DeviceId,
    node: CoordinatorId,
    scope: InviteScope,
    issued_at_ms: u64,
    expires_at_ms: u64,
    nonce: [u8; INVITE_NONCE_BYTES],
}

fn encode_body(body: &InviteBody) -> [u8; BODY_BYTES] {
    let mut out = [0u8; BODY_BYTES];
    let mut cursor = 0usize;
    out[cursor] = INVITE_SCHEMA_VERSION as u8;
    cursor += 1;
    out[cursor..cursor + 32].copy_from_slice(body.account_id.as_bytes());
    cursor += 32;
    out[cursor..cursor + 32].copy_from_slice(&body.account_public_key);
    cursor += 32;
    out[cursor..cursor + 32].copy_from_slice(body.device_id.as_bytes());
    cursor += 32;
    out[cursor..cursor + 32].copy_from_slice(body.node.as_bytes());
    cursor += 32;
    out[cursor] = body.scope.as_u8();
    cursor += 1;
    out[cursor..cursor + 8].copy_from_slice(&body.issued_at_ms.to_be_bytes());
    cursor += 8;
    out[cursor..cursor + 8].copy_from_slice(&body.expires_at_ms.to_be_bytes());
    cursor += 8;
    out[cursor..cursor + INVITE_NONCE_BYTES].copy_from_slice(&body.nonce);
    out
}

fn decode_body(bytes: &[u8]) -> Result<InviteBody> {
    if bytes.len() != BODY_BYTES {
        return Err(IdentityError::MalformedToken);
    }
    if bytes[0] != INVITE_SCHEMA_VERSION as u8 {
        return Err(IdentityError::UnsupportedTokenVersion);
    }
    let mut cursor = 1usize;
    let mut account_id_bytes = [0u8; 32];
    account_id_bytes.copy_from_slice(&bytes[cursor..cursor + 32]);
    cursor += 32;
    let mut account_public_key = [0u8; PUBLIC_KEY_BYTES];
    account_public_key.copy_from_slice(&bytes[cursor..cursor + 32]);
    cursor += 32;
    let mut device_id_bytes = [0u8; 32];
    device_id_bytes.copy_from_slice(&bytes[cursor..cursor + 32]);
    cursor += 32;
    let mut node_bytes = [0u8; 32];
    node_bytes.copy_from_slice(&bytes[cursor..cursor + 32]);
    cursor += 32;
    let scope = InviteScope::from_u8(bytes[cursor]).ok_or(IdentityError::WrongInviteScope)?;
    cursor += 1;
    let mut issued = [0u8; 8];
    issued.copy_from_slice(&bytes[cursor..cursor + 8]);
    cursor += 8;
    let mut expires = [0u8; 8];
    expires.copy_from_slice(&bytes[cursor..cursor + 8]);
    cursor += 8;
    let mut nonce = [0u8; INVITE_NONCE_BYTES];
    nonce.copy_from_slice(&bytes[cursor..cursor + INVITE_NONCE_BYTES]);
    Ok(InviteBody {
        account_id: AccountId::from_bytes(account_id_bytes),
        account_public_key,
        device_id: DeviceId::from_bytes(device_id_bytes),
        node: CoordinatorId::from_bytes(node_bytes),
        scope,
        issued_at_ms: u64::from_be_bytes(issued),
        expires_at_ms: u64::from_be_bytes(expires),
        nonce,
    })
}

fn invite_body_value(body: &InviteBody) -> WireValue {
    WireValue::List(vec![
        WireValue::U64(INVITE_SCHEMA_VERSION),
        WireValue::U64(u64::from(ProtocolVersion::V1.major)),
        WireValue::U64(u64::from(ProtocolVersion::V1.minor)),
        WireValue::id_bytes(body.account_id.as_bytes()),
        WireValue::Bytes(body.account_public_key.to_vec()),
        WireValue::id_bytes(body.device_id.as_bytes()),
        WireValue::id_bytes(body.node.as_bytes()),
        WireValue::U64(u64::from(body.scope.as_u8())),
        WireValue::U64(body.issued_at_ms),
        WireValue::U64(body.expires_at_ms),
        WireValue::Bytes(body.nonce.to_vec()),
    ])
}

fn signing_bytes(body: &InviteBody) -> Result<Vec<u8>> {
    let canonical = encode_canonical(&invite_body_value(body), ProtocolVersion::V1)?;
    let mut out = Vec::with_capacity(INVITE_SIGNING_DOMAIN.len() + canonical.len());
    out.extend_from_slice(INVITE_SIGNING_DOMAIN);
    out.extend_from_slice(canonical.as_bytes());
    Ok(out)
}

/// 不透明邀请口令文本。Debug 只打印前缀，泄不出口令内容。
#[derive(Clone, PartialEq, Eq)]
pub struct InviteToken(String);

impl InviteToken {
    /// 只读文本。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 取出文本。
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for InviteToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "InviteToken({}…{} bytes)", INVITE_TOKEN_VERSION, self.0.len())
    }
}

impl fmt::Display for InviteToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// IDENTITY-004：为「本账户 + 本设备 + 指定节点」生成一次性邀请口令。
///
/// 有效期必须在 [MIN_INVITE_TTL_MS, MAX_INVITE_TTL_MS] 内；签发时间不能为 0。
pub fn create_invite(
    account: &AccountKeys,
    device: &DeviceKeys,
    node: CoordinatorId,
    scope: InviteScope,
    issued_at_ms: u64,
    ttl_ms: u64,
) -> Result<InviteToken> {
    if issued_at_ms == 0 {
        return Err(IdentityError::InvalidTimestamp);
    }
    if ttl_ms == 0 || !(MIN_INVITE_TTL_MS..=MAX_INVITE_TTL_MS).contains(&ttl_ms) {
        return Err(IdentityError::InvalidInviteLifetime {
            max_ms: MAX_INVITE_TTL_MS,
        });
    }
    let expires_at_ms = issued_at_ms
        .checked_add(ttl_ms)
        .ok_or(IdentityError::InvalidInviteLifetime {
            max_ms: MAX_INVITE_TTL_MS,
        })?;
    let mut nonce = [0u8; INVITE_NONCE_BYTES];
    rand::fill(&mut nonce);
    let account_id = account.account_id();
    let device_public_key = device.public_key();
    let body = InviteBody {
        account_id,
        account_public_key: account.public_key(),
        device_id: device_id_for(account_id, &device_public_key),
        node,
        scope,
        issued_at_ms,
        expires_at_ms,
        nonce,
    };
    let message = signing_bytes(&body)?;
    let signature = account.sign(&message);
    let text = format!(
        "{}.{}.{}",
        INVITE_TOKEN_VERSION,
        hex_encode(&encode_body(&body)),
        hex_encode(&signature)
    );
    if text.len() > MAX_INVITE_TOKEN_BYTES {
        // 冻结布局不应触发；出现即视为内部不变量破坏。
        return Err(IdentityError::TokenTooLong {
            limit: MAX_INVITE_TOKEN_BYTES,
        });
    }
    Ok(InviteToken(text))
}

/// 校验邀请时的期望值。账户/节点/设备/信任锚全部可选，但给定即必须匹配。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InviteExpectations<'a> {
    /// 本地可信时间（UTC 毫秒）。
    pub now_ms: u64,
    /// 期望的协调节点。
    pub node: Option<CoordinatorId>,
    /// 期望的账户。
    pub account: Option<AccountId>,
    /// 期望的设备。
    pub device: Option<DeviceId>,
    /// 已固定信任锚；邀请自带公钥必须与它一致。
    pub trust: Option<&'a TrustAnchor>,
}

impl<'a> InviteExpectations<'a> {
    /// 只提供本地时间。
    pub const fn at(now_ms: u64) -> Self {
        Self {
            now_ms,
            node: None,
            account: None,
            device: None,
            trust: None,
        }
    }

    /// 要求节点匹配。
    pub const fn with_node(mut self, node: CoordinatorId) -> Self {
        self.node = Some(node);
        self
    }

    /// 要求账户匹配。
    pub const fn with_account(mut self, account: AccountId) -> Self {
        self.account = Some(account);
        self
    }

    /// 要求设备匹配。
    pub const fn with_device(mut self, device: DeviceId) -> Self {
        self.device = Some(device);
        self
    }

    /// 要求邀请公钥与已固定信任锚一致。
    pub const fn with_trust(mut self, trust: &'a TrustAnchor) -> Self {
        self.trust = Some(trust);
        self
    }
}

/// 已通过验签与期望检查的邀请摘要。只含公开材料。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedInvite {
    account_id: AccountId,
    account_public_key: [u8; PUBLIC_KEY_BYTES],
    device_id: DeviceId,
    node: CoordinatorId,
    scope: InviteScope,
    issued_at_ms: u64,
    expires_at_ms: u64,
    nonce: [u8; INVITE_NONCE_BYTES],
}

impl VerifiedInvite {
    /// 邀请账户。
    pub const fn account_id(&self) -> AccountId {
        self.account_id
    }

    /// 邀请账户公钥（可用于建立初始信任锚，必须由用户核验指纹）。
    pub const fn account_public_key(&self) -> &[u8; PUBLIC_KEY_BYTES] {
        &self.account_public_key
    }

    /// 被邀请设备。
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// 绑定的协调节点。
    pub const fn node(&self) -> CoordinatorId {
        self.node
    }

    /// 作用域。
    pub const fn scope(&self) -> InviteScope {
        self.scope
    }

    /// 签发时间。
    pub const fn issued_at_ms(&self) -> u64 {
        self.issued_at_ms
    }

    /// 失效时间。
    pub const fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }

    /// 一次性 nonce（持久化消费状态时用作邀请身份）。
    pub const fn nonce(&self) -> &[u8; INVITE_NONCE_BYTES] {
        &self.nonce
    }

    /// 邀请 ID 的规范文本（小写 hex，64 字符）；用于本地去重/撤销记录。
    pub fn invite_id_hex(&self) -> String {
        hex_encode(&self.nonce)
    }

    /// 账户人工核验指纹。
    pub fn account_fingerprint(&self) -> Fingerprint {
        Fingerprint::of_public_key(&self.account_public_key)
    }

    /// 本地时间是否已过期。
    pub const fn is_expired_at(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

/// 只做**口令信封**的结构解析（不验签、不返回任何身份字段）。
///
/// 供 UI 在粘贴/扫码后先判断「这是不是一个 inv1 口令、长度是否在限内」，
/// 但绝不因此信任其中身份：账户/设备/节点等字段只有在 [verify_invite] 通过后
/// 才可得。也正因为如此，本 crate 不提供「先解析、后验签」的第二套入口。
pub fn classify_invite_text(token: &str) -> Result<()> {
    if token.len() > MAX_INVITE_TOKEN_BYTES {
        return Err(IdentityError::TokenTooLong {
            limit: MAX_INVITE_TOKEN_BYTES,
        });
    }
    if !token.is_ascii() {
        return Err(IdentityError::MalformedToken);
    }
    let Some(rest) = token.strip_prefix(INVITE_TOKEN_VERSION) else {
        return Err(IdentityError::UnsupportedTokenVersion);
    };
    let Some(rest) = rest.strip_prefix('.') else {
        return Err(IdentityError::MalformedToken);
    };
    let mut parts = rest.split('.');
    let (Some(body_hex), Some(signature_hex), None) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(IdentityError::MalformedToken);
    };
    if body_hex.len() != BODY_HEX_CHARS || signature_hex.len() != SIGNATURE_HEX_CHARS {
        return Err(IdentityError::MalformedToken);
    }
    // hex 规范形式（小写、可解码）仍要检查，避免把显然损坏的文本当口令。
    hex_decode(body_hex)?;
    hex_decode(signature_hex)?;
    Ok(())
}

/// IDENTITY-005：解析并校验邀请口令，返回安全摘要。
///
/// 校验顺序：限长 → 前缀/结构 → hex 规范形式 → 正文长度/版本/作用域 →
/// 时间窗口 → 账户签名 → 信任锚 → 期望节点/账户/设备 → 过期。
pub fn verify_invite(token: &str, expectations: &InviteExpectations<'_>) -> Result<VerifiedInvite> {
    if token.len() > MAX_INVITE_TOKEN_BYTES {
        return Err(IdentityError::TokenTooLong {
            limit: MAX_INVITE_TOKEN_BYTES,
        });
    }
    if !token.is_ascii() {
        return Err(IdentityError::MalformedToken);
    }
    let Some(rest) = token.strip_prefix(INVITE_TOKEN_VERSION) else {
        return Err(IdentityError::UnsupportedTokenVersion);
    };
    let Some(rest) = rest.strip_prefix('.') else {
        return Err(IdentityError::MalformedToken);
    };
    let mut parts = rest.split('.');
    let (Some(body_hex), Some(signature_hex), None) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Err(IdentityError::MalformedToken);
    };
    if body_hex.len() != BODY_HEX_CHARS || signature_hex.len() != SIGNATURE_HEX_CHARS {
        return Err(IdentityError::MalformedToken);
    }
    let body_bytes = hex_decode(body_hex)?;
    let signature_bytes = hex_decode(signature_hex)?;
    let body = decode_body(&body_bytes)?;

    // 时间窗口自洽（不依赖本地时钟）。
    if body.issued_at_ms == 0 || body.expires_at_ms <= body.issued_at_ms {
        return Err(IdentityError::MalformedToken);
    }
    let lifetime = body.expires_at_ms - body.issued_at_ms;
    if !(MIN_INVITE_TTL_MS..=MAX_INVITE_TTL_MS).contains(&lifetime) {
        return Err(IdentityError::InvalidInviteLifetime {
            max_ms: MAX_INVITE_TTL_MS,
        });
    }
    // 账户自洽：账户 ID 必须由公钥派生。
    if account_id_from_public_key(&body.account_public_key) != body.account_id {
        return Err(IdentityError::AccountKeyMismatch);
    }
    // 先验签。
    let message = signing_bytes(&body)?;
    let mut signature = [0u8; SIGNATURE_BYTES];
    signature.copy_from_slice(&signature_bytes);
    verify_signature(&body.account_public_key, &message, &signature)?;
    // 信任锚：邀请不能静默替换已固定信任。
    if let Some(trust) = expectations.trust
        && (trust.account_id() != body.account_id
            || trust.account_public_key() != &body.account_public_key)
    {
        return Err(IdentityError::WrongInviteAccount);
    }
    if let Some(node) = expectations.node
        && node != body.node
    {
        return Err(IdentityError::WrongInviteNode);
    }
    if let Some(account) = expectations.account
        && account != body.account_id
    {
        return Err(IdentityError::WrongInviteAccount);
    }
    if let Some(device) = expectations.device
        && device != body.device_id
    {
        return Err(IdentityError::WrongInviteDevice);
    }
    // 最后看本地时间：避免伪造口令探测期限。
    if expectations.now_ms.saturating_add(INVITE_CLOCK_SKEW_MS) < body.issued_at_ms {
        return Err(IdentityError::InviteNotYetValid);
    }
    if expectations.now_ms >= body.expires_at_ms {
        return Err(IdentityError::InviteExpired);
    }
    Ok(VerifiedInvite {
        account_id: body.account_id,
        account_public_key: body.account_public_key,
        device_id: body.device_id,
        node: body.node,
        scope: body.scope,
        issued_at_ms: body.issued_at_ms,
        expires_at_ms: body.expires_at_ms,
        nonce: body.nonce,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KEY_SEED_BYTES;
    use chat_protocol::ErrorCode;

    const NODE_SEED: u8 = 0x33;
    const ISSUED_AT: u64 = 1_700_000_000_000;
    const TTL: u64 = 600_000;

    fn account() -> AccountKeys {
        AccountKeys::from_seed(&[0x01; KEY_SEED_BYTES])
    }

    fn device() -> DeviceKeys {
        DeviceKeys::from_seed(&[0x11; KEY_SEED_BYTES])
    }

    fn node(seed: u8) -> CoordinatorId {
        CoordinatorId::from_bytes([seed; 32])
    }

    fn token() -> InviteToken {
        create_invite(
            &account(),
            &device(),
            node(NODE_SEED),
            InviteScope::ContactRequest,
            ISSUED_AT,
            TTL,
        )
        .expect("invite")
    }

    fn replace_body_byte(token: &InviteToken, offset: usize, value: u8) -> String {
        let text = token.as_str();
        let mut parts = text.split('.');
        let prefix = parts.next().expect("prefix");
        let body_hex = parts.next().expect("body");
        let signature = parts.next().expect("signature");
        let mut body = hex_decode(body_hex).expect("body hex");
        body[offset] = value;
        format!("{}.{}.{}", prefix, hex_encode(&body), signature)
    }

    #[test]
    fn token_round_trip_carries_bound_scope() {
        let token = token();
        assert!(token.as_str().starts_with(INVITE_TOKEN_VERSION));
        assert!(token.as_str().len() <= MAX_INVITE_TOKEN_BYTES);
        let verified = verify_invite(
            token.as_str(),
            &InviteExpectations::at(ISSUED_AT)
                .with_node(node(NODE_SEED))
                .with_account(account().account_id())
                .with_device(device().device_id(account().account_id()))
                .with_trust(&account().trust_anchor()),
        )
        .expect("verified");
        assert_eq!(verified.account_id(), account().account_id());
        assert_eq!(verified.device_id(), device().device_id(account().account_id()));
        assert_eq!(verified.node(), node(NODE_SEED));
        assert_eq!(verified.scope(), InviteScope::ContactRequest);
        assert_eq!(verified.issued_at_ms(), ISSUED_AT);
        assert_eq!(verified.expires_at_ms(), ISSUED_AT + TTL);
        assert_eq!(verified.account_fingerprint(), account().fingerprint());
        assert_eq!(verified.account_public_key(), &account().public_key());
        assert_eq!(verified.invite_id_hex().len(), INVITE_NONCE_BYTES * 2);
        assert!(verified.invite_id_hex().chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(verified.invite_id_hex(), hex_encode(verified.nonce()));
        assert!(!verified.is_expired_at(ISSUED_AT + TTL - 1));
    }

    #[test]
    fn token_debug_does_not_leak_signature_or_body() {
        let token = token();
        let rendered = format!("{token:?}");
        assert!(rendered.contains(INVITE_TOKEN_VERSION));
        let signature_hex = token.as_str().rsplit('.').next().expect("signature");
        assert!(!rendered.contains(signature_hex));
    }

    #[test]
    fn envelope_classifier_accepts_real_tokens_only() {
        let token = token();
        classify_invite_text(token.as_str()).expect("real token envelope");
        assert_eq!(
            classify_invite_text("inv2.00.11").unwrap_err(),
            IdentityError::UnsupportedTokenVersion
        );
        assert_eq!(
            classify_invite_text("inv1.aabb.cc").unwrap_err(),
            IdentityError::MalformedToken
        );
        assert_eq!(
            classify_invite_text(&format!("inv1.{}", "A".repeat(BODY_HEX_CHARS))).unwrap_err(),
            IdentityError::MalformedToken
        );
        assert_eq!(
            classify_invite_text(&"a".repeat(MAX_INVITE_TOKEN_BYTES + 1)).unwrap_err(),
            IdentityError::TokenTooLong {
                limit: MAX_INVITE_TOKEN_BYTES
            }
        );
    }

    #[test]
    fn nonces_are_unique_per_invite() {
        let first = token();
        let second = token();
        assert_ne!(first.as_str(), second.as_str());
    }

    #[test]
    fn expired_invite_is_rejected_with_frozen_code() {
        let token = token();
        let error = verify_invite(token.as_str(), &InviteExpectations::at(ISSUED_AT + TTL))
            .expect_err("expired");
        assert_eq!(error, IdentityError::InviteExpired);
        assert_eq!(error.code(), ErrorCode::CursorExpired);
        assert_eq!(error.code().as_str(), "CURSOR_EXPIRED");
        assert!(!error.retryable());
    }

    #[test]
    fn invite_from_the_future_is_rejected_beyond_skew() {
        let token = token();
        assert!(
            verify_invite(
                token.as_str(),
                &InviteExpectations::at(ISSUED_AT - INVITE_CLOCK_SKEW_MS)
            )
            .is_ok()
        );
        assert_eq!(
            verify_invite(
                token.as_str(),
                &InviteExpectations::at(ISSUED_AT - INVITE_CLOCK_SKEW_MS - 1)
            )
            .unwrap_err(),
            IdentityError::InviteNotYetValid
        );
    }

    #[test]
    fn tampered_signature_bytes_are_rejected() {
        let token = token();
        let mut text = token.as_str().to_owned();
        let last = text.pop().expect("last char");
        text.push(if last == '0' { '1' } else { '0' });
        assert_eq!(
            verify_invite(&text, &InviteExpectations::at(ISSUED_AT)).unwrap_err(),
            IdentityError::InvalidSignature
        );
    }

    #[test]
    fn tampered_body_is_rejected() {
        let token = token();
        // 篡改 nonce（签名覆盖），应为验签失败。
        let nonce_offset = BODY_BYTES - INVITE_NONCE_BYTES;
        let tampered_nonce = replace_body_byte(&token, nonce_offset, 0x5a);
        assert_eq!(
            verify_invite(&tampered_nonce, &InviteExpectations::at(ISSUED_AT)).unwrap_err(),
            IdentityError::InvalidSignature
        );
        // 篡改账户 ID（先做自洽检查），应为账户绑定失败。
        let tampered_account = replace_body_byte(&token, 1, 0x00);
        assert_eq!(
            verify_invite(&tampered_account, &InviteExpectations::at(ISSUED_AT)).unwrap_err(),
            IdentityError::AccountKeyMismatch
        );
        // 篡改作用域（解码阶段即拒绝）。
        let tampered_scope = replace_body_byte(&token, BODY_BYTES - INVITE_NONCE_BYTES - 17, 0x7f);
        assert_eq!(
            verify_invite(&tampered_scope, &InviteExpectations::at(ISSUED_AT)).unwrap_err(),
            IdentityError::WrongInviteScope
        );
    }

    #[test]
    fn wrong_node_account_and_device_are_rejected() {
        let token = token();
        let base = InviteExpectations::at(ISSUED_AT);
        assert_eq!(
            verify_invite(token.as_str(), &base.with_node(node(0x44))).unwrap_err(),
            IdentityError::WrongInviteNode
        );
        assert_eq!(
            verify_invite(
                token.as_str(),
                &base.with_account(AccountKeys::from_seed(&[0x02; KEY_SEED_BYTES]).account_id())
            )
            .unwrap_err(),
            IdentityError::WrongInviteAccount
        );
        assert_eq!(
            verify_invite(
                token.as_str(),
                &base.with_device(DeviceId::from_bytes([0x44; 32]))
            )
            .unwrap_err(),
            IdentityError::WrongInviteDevice
        );
        let other_anchor =
            AccountKeys::from_seed(&[0x02; KEY_SEED_BYTES]).trust_anchor();
        assert_eq!(
            verify_invite(token.as_str(), &base.with_trust(&other_anchor)).unwrap_err(),
            IdentityError::WrongInviteAccount
        );
    }

    #[test]
    fn lifetime_bounds_are_enforced() {
        for ttl in [0, 1, MIN_INVITE_TTL_MS - 1, MAX_INVITE_TTL_MS + 1, u64::MAX] {
            assert!(
                create_invite(
                    &account(),
                    &device(),
                    node(NODE_SEED),
                    InviteScope::ContactRequest,
                    ISSUED_AT,
                    ttl
                )
                .is_err(),
                "ttl {ttl} must be rejected"
            );
        }
        for ttl in [MIN_INVITE_TTL_MS, MAX_INVITE_TTL_MS] {
            assert!(
                create_invite(
                    &account(),
                    &device(),
                    node(NODE_SEED),
                    InviteScope::ContactRequest,
                    ISSUED_AT,
                    ttl
                )
                .is_ok(),
                "ttl {ttl} must be accepted"
            );
        }
        assert_eq!(
            create_invite(
                &account(),
                &device(),
                node(NODE_SEED),
                InviteScope::ContactRequest,
                0,
                TTL
            )
            .unwrap_err(),
            IdentityError::InvalidTimestamp
        );
        assert_eq!(
            create_invite(
                &account(),
                &device(),
                node(NODE_SEED),
                InviteScope::ContactRequest,
                u64::MAX - 1,
                MAX_INVITE_TTL_MS
            )
            .unwrap_err(),
            IdentityError::InvalidInviteLifetime {
                max_ms: MAX_INVITE_TTL_MS
            }
        );
    }

    #[test]
    fn malformed_tokens_are_bounded_and_rejected() {
        let base = InviteExpectations::at(ISSUED_AT);
        let oversized = format!("inv1.{}", "a".repeat(MAX_INVITE_TOKEN_BYTES));
        assert_eq!(
            verify_invite(&oversized, &base).unwrap_err(),
            IdentityError::TokenTooLong {
                limit: MAX_INVITE_TOKEN_BYTES
            }
        );
        assert_eq!(
            verify_invite("inv2.00.11", &base).unwrap_err(),
            IdentityError::UnsupportedTokenVersion
        );
        for malformed in [
            "",
            "inv1",
            "inv1.",
            "inv1..",
            "inv1.aabb.",
            "inv1.aabb.cc",
            "INV1.00.11",
            "inv1.中.文",
        ] {
            let error = verify_invite(malformed, &base).expect_err("malformed token");
            assert!(
                matches!(
                    error,
                    IdentityError::MalformedToken
                        | IdentityError::UnsupportedTokenVersion
                        | IdentityError::TokenTooLong { .. }
                ),
                "unexpected error {error:?} for {malformed:?}"
            );
        }
        // 大写十六进制不是规范形式（只有 hex 部分大写，前缀仍是 inv1）。
        let token = token();
        let mut parts = token.as_str().split('.');
        let prefix = parts.next().expect("prefix");
        let body_hex = parts.next().expect("body").to_uppercase();
        let signature_hex = parts.next().expect("signature");
        let uppercased = format!("{prefix}.{body_hex}.{signature_hex}");
        assert_eq!(
            verify_invite(&uppercased, &base).unwrap_err(),
            IdentityError::MalformedToken
        );
    }
}
