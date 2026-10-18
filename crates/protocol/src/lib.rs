//! 线协议、规范编码、有界解码、验签原文、认证游标、日志链、应用事件与能力协商。
//!
//! 模块边界：
//!
//! - [`wire`]：P0 隔离帧（保持既有公开 API 不变）；
//! - [`ids`]：PROTOCOL-001 强类型标识符；
//! - [`canonical`]：PROTOCOL-002 确定性规范编码；
//! - [`decode`]：PROTOCOL-003 有界解码；
//! - [`signing`]：PROTOCOL-004/006 验签原文字节；
//! - [`verify`]：PROTOCOL-005/007 候选验签与日志哈希链；
//! - [`event`]：PROTOCOL-008/009 应用事件编码与内外绑定；
//! - [`extension`]：PROTOCOL-010 扩展内容校验；
//! - [`capability`]：PROTOCOL-011 能力协商；
//! - [`cursor`]：PROTOCOL-012/013 认证分页游标；
//! - [`error`]：PROTOCOL-014 稳定错误码与脱敏映射。
//!
//! 所有函数是纯函数：不读网络、不读数据库、不持有生产私钥。安全验证失败一律
//! 显式失败，绝不「模拟成功」。
#![forbid(unsafe_code)]
pub mod wire;

pub mod canonical;
pub mod capability;
pub mod cursor;
pub mod decode;
pub mod digest;
pub mod error;
pub mod event;
pub mod extension;
pub mod ids;
pub mod signing;
pub mod verify;

pub use canonical::{
    CANONICAL_MAGIC, CanonicalBytes, KNOWN_FIELD_IDS, MAX_CANONICAL_BYTES, MAX_CANONICAL_DEPTH,
    MAX_CANONICAL_TEXT_BYTES, MAX_COLLECTION_ITEMS, ProtocolVersion, WireField, WireValue,
    encode_canonical, field, is_known_field,
};
pub use capability::{
    CAPABILITY_SCHEMA_VERSION, Capabilities, CapabilityError, CapabilityLimits,
    MAX_CAPABILITY_FEATURES, MAX_CAPABILITY_NAME_BYTES, MAX_DECLARED_LIMIT, MAX_DECLARED_VERSIONS,
    REQUIRED_FEATURES, SessionCapabilities, negotiate_capabilities,
};
pub use cursor::{
    CURSOR_MAX_CLOCK_SKEW_MS, CURSOR_TIE_BYTES, CURSOR_TOKEN_VERSION, CURSOR_TTL_MS, CursorError,
    CursorKey, CursorPosition, CursorResource, CursorScope, MAX_CURSOR_SEQ,
    MAX_CURSOR_SORT_KEY_BYTES, MAX_CURSOR_TOKEN_BYTES, OpaqueCursor, decode_page_cursor,
    encode_page_cursor,
};
pub use decode::{DecodeLimits, MalformedFrame, WireFrame, decode_bounded};
pub use digest::{ControlHead, DIGEST_BYTES, Digest};
pub use error::{
    DEFAULT_RATE_LIMIT_RETRY_MS, ErrorCode, IdError, InternalError, MAX_RETRY_AFTER_MS,
    ProtocolError, PublicError, Result, map_error,
};
pub use event::{
    ApplicationContent, AttachmentRef, AuthenticatedEvent, DecryptedEvent, EVENT_DIGEST_DOMAIN,
    MAX_APPLICATION_EVENT_BYTES, MAX_EVENT_TEXT_BYTES, MessageOrigin, MlsSender,
    PlainApplicationEvent, ReplyTarget, SecretBytes, decode_application_event,
    encode_application_event, verify_inner_event,
};
pub use extension::{
    ContentLimits, ExtensionContent, ExtensionError, MAX_EXTENSION_PAYLOAD_BYTES,
    MAX_FALLBACK_BYTES, MAX_NAMESPACE_BYTES, ValidatedExtension, validate_extension,
};
pub use ids::{
    AccountId, ConversationId, CoordinatorId, DeviceId, DraftId, EventId, GroupId,
    ID_PAYLOAD_BYTES, ID_VERSION, IdKind, MAX_ID_TEXT_BYTES, MessageId, ObjectId, OperationId,
    SubmissionId, TypedId, parse_id,
};
pub use signing::{
    CANDIDATE_SIGNING_DOMAIN, CandidateKind, ControlMetadata, ControlOperation,
    MAX_CIPHERTEXT_BYTES, RECEIPT_SIGNING_DOMAIN, SigningKey, UnsignedCandidate,
    candidate_signing_bytes, receipt_signing_bytes, sign_bytes, signing_key_from_seed,
    verify_ed25519,
};
pub use verify::{
    AcceptedLogEntry, GENESIS_SEQUENCE, GroupContext, LOG_ENTRY_HASH_DOMAIN, PinnedNodeKey,
    SignedCandidate, VerifiedCandidate, VerifiedCredential, VerifiedHead, VerifiedLogEntry,
    log_entry_hash, verify_candidate, verify_log_entry,
};
