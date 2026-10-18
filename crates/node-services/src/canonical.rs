//! 节点鉴权与注册的规范编码与有界校验原语。
//!
//! 对应 docs/implementation/api.md 中 PROTOCOL-002 encode_canonical 的节点侧实现：
//! 手写“域分隔 + 长度前缀”编码，绝不签名键顺序不定的 JSON。所有长度前缀为 2 字节大端，
//! 固定长度字段（公钥、摘要、签名）不带前缀直接拼接。
use crate::error::{NodeError, NodeResult};
use crate::limits::{MAX_TIMESTAMP};
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// 挑战签名域。
pub const CHALLENGE_DOMAIN: &[u8] = b"chat/p1/node-auth-challenge/v1";
/// 请求鉴权签名域。
pub const REQUEST_DOMAIN: &[u8] = b"chat/p1/node-auth-request/v1";
/// 设备凭据签名域（账户主管钥签发）。
pub const CREDENTIAL_DOMAIN: &[u8] = b"chat/p1/node-device-credential/v1";
/// 注册能力签名域（账户主管钥签发）。
pub const CAPABILITY_DOMAIN: &[u8] = b"chat/p1/node-registration-capability/v1";
/// 节点注册回执签名域。
pub const REGISTRATION_DOMAIN: &[u8] = b"chat/p1/node-device-registration/v1";
/// 群创建请求签名域（创建设备用私钥签，节点用已登记公钥验）。
pub const GROUP_CREATION_DOMAIN: &[u8] = b"chat/p1/node-group-creation/v1";
/// 群创建回执签名域（节点签发）。
pub const GROUP_REGISTRATION_DOMAIN: &[u8] = b"chat/p1/node-group-registration/v1";
/// 群日志 head 签名域（与候选回执域严格区分）。
pub const LOG_HEAD_DOMAIN: &[u8] = b"chat/p1/node-log-head/v1";
/// 群日志链摘要域：log_hash = H(域 || group || seq || previous_hash || candidate_hash)。
pub const LOG_CHAIN_DOMAIN: &[u8] = b"chat/p1/node-log-chain/v1";
/// 信箱存储回执签名域（节点签发）。
pub const MAILBOX_RECEIPT_DOMAIN: &[u8] = b"chat/p1/node-mailbox-receipt/v1";
/// 附件上传会话签名域（节点签发的“受限上传条款”）。
pub const BLOB_SESSION_DOMAIN: &[u8] = b"chat/p1/node-blob-session/v1";
/// 附件分块回执签名域。
pub const BLOB_CHUNK_RECEIPT_DOMAIN: &[u8] = b"chat/p1/node-blob-chunk-receipt/v1";
/// 附件发布回执签名域（与分块回执严格区分）。
pub const BLOB_PUBLISH_RECEIPT_DOMAIN: &[u8] = b"chat/p1/node-blob-publish-receipt/v1";

/// 分页游标派生密钥域（由节点签名种子派生，绝不外泄）。
pub const CURSOR_KEY_DOMAIN: &[u8] = b"chat/p1/node-cursor-key/v1";
/// 节点侧主体派生域：把节点登记的有界标识绑定到协议 32 字节 ID。
pub const NODE_ACCOUNT_DERIVE_DOMAIN: &[u8] = b"chat/p1/node-account-derive/v1";
pub const NODE_DEVICE_DERIVE_DOMAIN: &[u8] = b"chat/p1/node-device-derive/v1";

/// 有界变长拼接摘要：按顺序拼接全部片段后做一次 SHA-256。
///
/// 只用于派生固定长度的绑定值（游标密钥、游标作用域 ID），不用于签名原文；
/// 签名原文一律使用带长度前缀的 push_field，避免拼接歧义。
pub fn sha256_concat(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// SHA-256 摘要。
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// 小写十六进制编码。
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    out
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 严格解码固定长度十六进制字段；长度或字符不符即拒绝。
pub fn hex_decode<const N: usize>(field: &'static str, value: &str) -> NodeResult<[u8; N]> {
    if value.len() != N * 2 {
        return Err(NodeError::InvalidRequest(field));
    }
    let raw = value.as_bytes();
    let mut out = [0_u8; N];
    for (index, slot) in out.iter_mut().enumerate() {
        let high = hex_nibble(raw[index * 2]).ok_or(NodeError::InvalidRequest(field))?;
        let low = hex_nibble(raw[index * 2 + 1]).ok_or(NodeError::InvalidRequest(field))?;
        *slot = (high << 4) | low;
    }
    Ok(out)
}

/// 读取 SQLite 列里的 32 字节摘要；长度不符即视为存储损坏（显式 STORAGE_FAILURE）。
pub fn bytes32(field: &'static str, bytes: Vec<u8>) -> NodeResult<[u8; 32]> {
    let _ = field;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| NodeError::StorageUnavailable)
}

/// 严格解码可变长度十六进制字段（例如候选密文）；调用方先限制最大长度。
///
/// 拒绝奇数长度、非十六进制字符与超过 `max_bytes` 的输入，绝不截断。
pub fn decode_hex_bytes(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> NodeResult<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(NodeError::InvalidRequest(field));
    }
    let bytes = value.len() / 2;
    if bytes > max_bytes {
        return Err(NodeError::PayloadTooLarge);
    }
    let raw = value.as_bytes();
    let mut out = Vec::with_capacity(bytes);
    for index in 0..bytes {
        let high = hex_nibble(raw[index * 2]).ok_or(NodeError::InvalidRequest(field))?;
        let low = hex_nibble(raw[index * 2 + 1]).ok_or(NodeError::InvalidRequest(field))?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

/// 追加 2 字节大端长度前缀字段。调用方必须先用 validate_bounded_id 限制长度。
pub fn push_field(out: &mut Vec<u8>, field: &[u8]) {
    debug_assert!(field.len() <= u16::MAX as usize);
    out.extend_from_slice(&(field.len() as u16).to_be_bytes());
    out.extend_from_slice(field);
}

/// 标识字符集：ASCII 字母数字加 `._:-`；不允许空白、控制符与非 ASCII。
pub fn validate_bounded_id(kind: &'static str, value: &str, max_bytes: usize) -> NodeResult<()> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(NodeError::InvalidRequest(kind));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(NodeError::InvalidRequest(kind));
    }
    Ok(())
}

/// 时间戳必须能安全写入 SQLite INTEGER。
pub fn validate_timestamp(field: &'static str, value: u64) -> NodeResult<()> {
    if value > MAX_TIMESTAMP {
        return Err(NodeError::InvalidRequest(field));
    }
    Ok(())
}

/// 规范化 HTTP 方法：ASCII 大写，长度受限。
pub fn canonical_method(method: &str) -> NodeResult<String> {
    if method.is_empty() || method.len() > crate::limits::MAX_METHOD_BYTES {
        return Err(NodeError::InvalidRequest("method"));
    }
    if !method.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(NodeError::InvalidRequest("method"));
    }
    Ok(method.to_ascii_uppercase())
}

/// 规范化请求路径：必须以 `/` 开头，禁止查询/片段/百分号编码与控制字符。
///
/// 拒绝 `%` 与 `?`/`#` 是为了让“线上原文”与“签名原文”只有唯一写法，
/// 不给百分号规范化留出跨实现的歧义空间。
pub fn canonical_path(path: &str) -> NodeResult<String> {
    if path.is_empty() || !path.starts_with('/') || path.len() > crate::limits::MAX_PATH_BYTES {
        return Err(NodeError::InvalidRequest("path"));
    }
    if path
        .bytes()
        .any(|byte| !(0x21..=0x7e).contains(&byte) || matches!(byte, b'%' | b'?' | b'#' | b'\\'))
    {
        return Err(NodeError::InvalidRequest("path"));
    }
    // 拒绝 "." / ".." 段：节点固定路由不需要相对段，也不给路径穿越留解释空间。
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(NodeError::InvalidRequest("path"));
    }
    Ok(path.to_owned())
}

/// 规范化查询串：允许为空；禁止片段与不可打印字符。
pub fn canonical_query(query: &str) -> NodeResult<String> {
    if query.len() > crate::limits::MAX_QUERY_BYTES {
        return Err(NodeError::InvalidRequest("query"));
    }
    if query
        .bytes()
        .any(|byte| !(0x21..=0x7e).contains(&byte) || byte == b'#')
    {
        return Err(NodeError::InvalidRequest("query"));
    }
    Ok(query.to_owned())
}

/// 用给定公钥严格验证 Ed25519 签名；失败时返回调用方指定的错误（不同用途错误码不同）。
pub fn verify_ed25519(
    public_key: &[u8; 32],
    message: &[u8],
    signature: &[u8; 64],
    on_error: impl Fn() -> NodeError,
) -> NodeResult<()> {
    let key = VerifyingKey::from_bytes(public_key).map_err(|_| on_error())?;
    let signature = Signature::from_bytes(signature);
    key.verify_strict(message, &signature).map_err(|_| on_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::MAX_PATH_BYTES;

    #[test]
    fn hex_round_trip_and_strict_length() {
        let bytes = [0_u8, 1, 0xab, 0xff];
        let encoded = hex_encode(&bytes);
        assert_eq!(encoded, "0001abff");
        assert_eq!(hex_decode::<4>("nonce", &encoded).unwrap(), bytes);
        assert_eq!(hex_decode::<4>("nonce", "0001ABFF").unwrap(), bytes);
        assert!(hex_decode::<4>("nonce", "0001ab").is_err());
        assert!(hex_decode::<4>("nonce", "0001abfg").is_err());
        assert!(hex_decode::<4>("nonce", "").is_err());
    }

    #[test]
    fn id_and_timestamp_bounds_are_enforced() {
        assert!(validate_bounded_id("device id", "dev-1", 128).is_ok());
        assert!(validate_bounded_id("device id", "dev 1", 128).is_err());
        assert!(validate_bounded_id("device id", "", 128).is_err());
        assert!(validate_bounded_id("device id", &"a".repeat(129), 128).is_err());
        assert!(validate_bounded_id("device id", "设备", 128).is_err());
        assert!(validate_timestamp("now", MAX_TIMESTAMP).is_ok());
        assert!(validate_timestamp("now", MAX_TIMESTAMP + 1).is_err());
    }

    #[test]
    fn path_and_method_are_canonicalized_or_rejected() {
        assert_eq!(canonical_method("post").unwrap(), "POST");
        assert!(canonical_method("PO ST").is_err());
        assert!(canonical_method("").is_err());
        assert_eq!(canonical_path("/v1/devices").unwrap(), "/v1/devices");
        assert!(canonical_path("v1/devices").is_err());
        assert!(canonical_path("/v1/devices%2f").is_err());
        assert!(canonical_path("/v1/devices?x=1").is_err());
        assert!(canonical_path("/v1/..").is_err());
        assert!(canonical_path(&"a".repeat(MAX_PATH_BYTES + 1)).is_err());
        assert_eq!(canonical_query("a=1&b=2").unwrap(), "a=1&b=2");
        assert_eq!(canonical_query("").unwrap(), "");
        assert!(canonical_query("a=1#frag").is_err());
    }

    #[test]
    fn length_prefixed_fields_are_unambiguous() {
        let mut left = Vec::new();
        push_field(&mut left, b"ab");
        push_field(&mut left, b"c");
        let mut right = Vec::new();
        push_field(&mut right, b"a");
        push_field(&mut right, b"bc");
        assert_ne!(left, right);
        assert_eq!(&left[..2], &[0, 2]);
    }
}
