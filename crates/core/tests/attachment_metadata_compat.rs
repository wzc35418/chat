//! 字段 57 双版本兼容窗口：v1 读 v2 不报错、v2 用新名字、未知版本与真损坏仍拒绝。
//!
//! 字节级用例（不依赖附件夹具）：直接构造已登记的 v1/v2 布局。

use chat_core::attachments::{
    ATTACHMENT_METADATA_DISPLAY_NAME, ATTACHMENT_METADATA_VERSIONS, ATTACHMENT_METADATA_VERSION,
    ATTACHMENT_METADATA_VERSION_DISPLAY_NAME, MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES,
    attachment_metadata_version_supported, parse_attachment_metadata,
};

const DESCRIPTOR: [u8; 4] = [0xde, 0xad, 0xbe, 0xef];

/// 构造 v1 主体（所有版本共享的前缀 + binding），version 字节由调用方决定。
fn v1_payload_with_version(version: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(143);
    out.push(version);
    out.extend_from_slice(&[0x01; 16]); // file_id
    out.extend_from_slice(&9_u64.to_be_bytes()); // plain_size
    out.extend_from_slice(&11_u64.to_be_bytes()); // ciphertext_bytes
    out.extend_from_slice(&4_u32.to_be_bytes()); // chunk_bytes
    out.extend_from_slice(&1_u32.to_be_bytes()); // chunk_count
    out.extend_from_slice(&[0x02; 32]); // content_digest
    out.extend_from_slice(&[0x03; 32]); // ciphertext_digest
    out.extend_from_slice(&(DESCRIPTOR.len() as u16).to_be_bytes());
    out.extend_from_slice(&DESCRIPTOR);
    out.extend_from_slice(&[0x04; 32]); // binding
    out
}

/// v2 = v1 主体（版本号 2）+ 尾部 `name_len ‖ name`。
fn v2_payload(name: &str) -> Vec<u8> {
    let mut out = v1_payload_with_version(ATTACHMENT_METADATA_VERSION_DISPLAY_NAME);
    out.push(u8::try_from(name.len()).expect("name fits u8"));
    out.extend_from_slice(name.as_bytes());
    out
}

#[test]
fn a_v1_view_accepts_v2_bytes_and_falls_back_to_the_placeholder() {
    let v2 = v2_payload("会后纪要.pdf");
    // v2 解析：用尾部段里的名字。
    let parsed = parse_attachment_metadata(&v2).expect("v2 metadata parses");
    assert_eq!(parsed.display_name, "会后纪要.pdf");
    // v1 视角（把版本字节改回 1、尾部多余字节保留）：必须**忽略**尾部、回退占位名、不报错。
    let mut as_v1 = v2.clone();
    as_v1[0] = ATTACHMENT_METADATA_VERSION;
    assert!(as_v1.len() > 143, "the v2 trailer is still there");
    let legacy_view = parse_attachment_metadata(&as_v1).expect("a v1 parser must tolerate the trailer");
    assert_eq!(legacy_view.display_name, ATTACHMENT_METADATA_DISPLAY_NAME);
    // 主体字段在两种视角下完全一致（尾部段不影响 v1 语义）。
    assert_eq!(legacy_view.file_id, parsed.file_id);
    assert_eq!(legacy_view.binding, parsed.binding);
    assert_eq!(legacy_view.chunk_count, parsed.chunk_count);
}

#[test]
fn plain_v1_metadata_is_unchanged() {
    let v1 = v1_payload_with_version(ATTACHMENT_METADATA_VERSION);
    let parsed = parse_attachment_metadata(&v1).expect("v1 metadata parses");
    assert_eq!(parsed.display_name, ATTACHMENT_METADATA_DISPLAY_NAME);
    assert_eq!(parsed.plain_size, 9);
    assert_eq!(parsed.ciphertext_bytes, 11);
    // v1 主体后跟任意多余字节（发送方是 v2 或未来附加段）都不影响 v1 解析。
    let mut padded = v1.clone();
    padded.extend_from_slice(&[0xff; 9]);
    let parsed = parse_attachment_metadata(&padded).expect("v1 tolerates trailing bytes");
    assert_eq!(parsed.display_name, ATTACHMENT_METADATA_DISPLAY_NAME);
    assert_eq!(parsed.binding, hex(&[0x04; 32]));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn unknown_versions_are_rejected_and_are_not_registered() {
    assert_eq!(ATTACHMENT_METADATA_VERSIONS, [1, 2]);
    assert!(attachment_metadata_version_supported(ATTACHMENT_METADATA_VERSION));
    assert!(attachment_metadata_version_supported(
        ATTACHMENT_METADATA_VERSION_DISPLAY_NAME
    ));
    assert!(!attachment_metadata_version_supported(3));
    for version in [0_u8, 3, 9, 255] {
        let bytes = v1_payload_with_version(version);
        let error = parse_attachment_metadata(&bytes).expect_err("unknown version is rejected");
        assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    }
}

#[test]
fn v2_trailer_bounds_and_shape_are_still_enforced() {
    // 1) 名字长度前缀超过 128 → 真损坏，拒绝。
    let mut too_long = v1_payload_with_version(ATTACHMENT_METADATA_VERSION_DISPLAY_NAME);
    too_long.push((MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES + 1) as u8);
    too_long.extend_from_slice(&[0x61; MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES + 1]);
    let error = parse_attachment_metadata(&too_long).expect_err("name over the limit");
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    // 2) 前缀声明的长度与实际尾部不符（多一字节）。
    let mut extra = v2_payload("abc.txt");
    extra.push(0x00);
    let error = parse_attachment_metadata(&extra).expect_err("trailing byte");
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    // 3) 前缀声明的长度比实际短（截断最后一位）。
    let mut short = v2_payload("abcdef.txt");
    short.pop();
    let error = parse_attachment_metadata(&short).expect_err("truncated name");
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    // 4) v2 完全没有尾部段。
    let bare = v1_payload_with_version(ATTACHMENT_METADATA_VERSION_DISPLAY_NAME);
    let error = parse_attachment_metadata(&bare).expect_err("v2 without a name segment");
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    // 5) 名字不是 UTF-8。
    let mut invalid_utf8 = v1_payload_with_version(ATTACHMENT_METADATA_VERSION_DISPLAY_NAME);
    invalid_utf8.push(2);
    invalid_utf8.extend_from_slice(&[0xff, 0xfe]);
    let error = parse_attachment_metadata(&invalid_utf8).expect_err("invalid UTF-8 name");
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    // 6) 合法的边界：128 字节名字可以解析。
    let name = "n".repeat(MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES);
    let parsed = parse_attachment_metadata(&v2_payload(&name)).expect("128 bytes is accepted");
    assert_eq!(parsed.display_name.len(), MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES);
}
