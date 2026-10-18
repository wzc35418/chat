//! SecretDescriptor（附件 FileKey 的 E2EE 封装）用例：往返、错密钥/错附件拒绝、
//! 篡改与截断拒绝、描述符不含明文密钥。

use chat_crypto::attachment::{FileKey, AttachmentCryptoError};
use chat_crypto::secret_descriptor::{
    SECRET_DESCRIPTOR_BODY_BYTES, SECRET_DESCRIPTOR_BYTES, SecretDescriptor, open_secret_descriptor,
    seal_secret_descriptor,
};

fn wrapping_key(seed: u8) -> [u8; 32] {
    [seed; 32]
}

fn error_code(error: AttachmentCryptoError) -> &'static str {
    error.code()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn secret_descriptor_roundtrip_opens_the_same_file_key() {
    let file_key = FileKey::generate();
    let descriptor = seal_secret_descriptor(&wrapping_key(0x11), "att-00112233445566778899aabbccddeeff", &file_key)
        .expect("seal descriptor");
    assert_eq!(descriptor.version(), 1);
    assert_eq!(descriptor.as_bytes().len(), SECRET_DESCRIPTOR_BYTES);
    assert_eq!(descriptor.digest().len(), 32);

    let opened = open_secret_descriptor(
        &wrapping_key(0x11),
        "att-00112233445566778899aabbccddeeff",
        &descriptor,
    )
    .expect("open descriptor");
    assert!(opened.equals(&file_key), "round trip must restore the same file key");

    // 重新解析（模拟经 E2EE 事件传输）后仍可打开。
    let reparsed = SecretDescriptor::from_bytes(descriptor.as_bytes().to_vec()).expect("reparse");
    let opened_again = open_secret_descriptor(
        &wrapping_key(0x11),
        "att-00112233445566778899aabbccddeeff",
        &reparsed,
    )
    .expect("open reparsed");
    assert!(opened_again.equals(&file_key));

    // 同一密钥两次封装：nonce 不同 → 描述符不同（不可当稳定 ID 用）。
    let second = seal_secret_descriptor(&wrapping_key(0x11), "att-00112233445566778899aabbccddeeff", &file_key)
        .expect("seal again");
    assert_ne!(descriptor.as_bytes(), second.as_bytes());
}

#[test]
fn secret_descriptor_rejects_wrong_wrapping_key_and_attachment_id() {
    let file_key = FileKey::generate();
    let descriptor = seal_secret_descriptor(&wrapping_key(0x11), "att-aaaa", &file_key).expect("seal");
    // 错包装密钥。
    let wrong = open_secret_descriptor(&wrapping_key(0x22), "att-aaaa", &descriptor).unwrap_err();
    assert_eq!(error_code(wrong), "ATTACHMENT_CORRUPTED");
    // 换附件 ID（AAD 绑定）。
    let moved = open_secret_descriptor(&wrapping_key(0x11), "att-bbbb", &descriptor).unwrap_err();
    assert_eq!(error_code(moved), "ATTACHMENT_CORRUPTED");
    // 空/超长 attachment_id 在封装阶段就明确拒绝。
    assert_eq!(
        error_code(seal_secret_descriptor(&wrapping_key(0x11), "", &file_key).unwrap_err()),
        "INVALID_CHUNK_CONTEXT"
    );
    let long_id = "x".repeat(129);
    assert_eq!(
        error_code(seal_secret_descriptor(&wrapping_key(0x11), &long_id, &file_key).unwrap_err()),
        "INVALID_CHUNK_CONTEXT"
    );
}

#[test]
fn secret_descriptor_rejects_tampered_truncated_and_version_mismatch() {
    let file_key = FileKey::generate();
    let descriptor = seal_secret_descriptor(&wrapping_key(0x33), "att-cccc", &file_key).expect("seal");
    let original = descriptor.as_bytes().to_vec();

    // 篡改密文正文：摘要先失配。
    let mut tampered = original.clone();
    tampered[SECRET_DESCRIPTOR_BODY_BYTES - 1] ^= 0xff;
    let parsed = SecretDescriptor::from_bytes(tampered).expect("length still valid");
    assert_eq!(
        error_code(open_secret_descriptor(&wrapping_key(0x33), "att-cccc", &parsed).unwrap_err()),
        "ATTACHMENT_CORRUPTED"
    );

    // 篡改摘要本身。
    let mut digest_tampered = original.clone();
    let digest_start = SECRET_DESCRIPTOR_BODY_BYTES;
    digest_tampered[digest_start] ^= 0xff;
    let parsed = SecretDescriptor::from_bytes(digest_tampered).expect("length still valid");
    assert_eq!(
        error_code(open_secret_descriptor(&wrapping_key(0x33), "att-cccc", &parsed).unwrap_err()),
        "ATTACHMENT_CORRUPTED"
    );

    // 篡改版本字节：解析阶段拒绝（未识别版本）。
    let mut version_tampered = original.clone();
    version_tampered[0] = 2;
    assert!(SecretDescriptor::from_bytes(version_tampered).is_err());

    // 截断/加长：解析阶段拒绝（有界长度）。
    let mut truncated = original.clone();
    truncated.pop();
    assert!(SecretDescriptor::from_bytes(truncated).is_err());
    let mut extended = original.clone();
    extended.push(0);
    assert!(SecretDescriptor::from_bytes(extended).is_err());
    assert!(SecretDescriptor::from_bytes(vec![0u8; 1024]).is_err());
}

#[test]
fn secret_descriptor_contains_no_plaintext_key() {
    let file_key = FileKey::generate();
    let key_bytes = file_key.copy_bytes();
    let descriptor = seal_secret_descriptor(&wrapping_key(0x44), "att-dddd", &file_key).expect("seal");
    assert!(
        !contains(descriptor.as_bytes(), key_bytes.as_slice()),
        "descriptor must not contain the plaintext file key"
    );
    // 描述符 Debug 只给版本与摘要，不输出密钥字段。
    let debug = format!("{descriptor:?}");
    assert!(debug.contains("SecretDescriptor"));
    assert!(debug.contains("version"));
    assert!(!debug.contains("file_key"));
}
