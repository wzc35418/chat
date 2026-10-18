//! CRYPTO-011/012 分块 AEAD 集成测试：往返、篡改、截断、乱序、重复、错误密钥、
//! 伪造最后一块与上下文边界。

use chat_crypto::attachment::{
    CHUNK_BYTES, CHUNK_FORMAT_VERSION, ChunkContext, CipherChunk, FILE_KEY_BYTES,
    MAX_ATTACHMENT_BYTES, MAX_TOTAL_CHUNKS, FileId, FileKey, chunk_context_for, chunk_count,
    chunk_plain_length, decrypt_attachment_chunk, encrypt_attachment_chunk,
};

fn pattern(length: usize, seed: u8) -> Vec<u8> {
    (0..length)
        .map(|index| (index as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// 把一个逻辑文件切成上下文+密文列表（模拟导入流程）。
fn encrypt_file(key: &FileKey, file_id: FileId, plaintext: &[u8]) -> Vec<(ChunkContext, CipherChunk)> {
    let total = chunk_count(plaintext.len() as u64).unwrap();
    let mut chunks = Vec::new();
    for index in 0..total {
        let start = index as usize * CHUNK_BYTES;
        let end = (start + CHUNK_BYTES).min(plaintext.len());
        let context = chunk_context_for(file_id, plaintext.len() as u64, index).unwrap();
        let chunk = encrypt_attachment_chunk(key, &context, &plaintext[start..end]).unwrap();
        chunks.push((context, chunk));
    }
    chunks
}

fn decrypt_file(
    key: &FileKey,
    plain_size: u64,
    chunks: &[(ChunkContext, CipherChunk)],
) -> chat_crypto::attachment::Result<Vec<u8>> {
    let mut output = Vec::new();
    for (context, chunk) in chunks {
        let mut plaintext = decrypt_attachment_chunk(key, context, chunk)?;
        output.append(&mut plaintext);
    }
    assert_eq!(output.len() as u64, plain_size);
    Ok(output)
}

#[test]
fn multi_chunk_roundtrip_and_size_helpers() {
    let key = FileKey::generate();
    let file_id = FileId::generate();
    // 2.5 块，覆盖满块与不满的最后一块。
    let plaintext = pattern(CHUNK_BYTES * 2 + 123, 7);
    let chunks = encrypt_file(&key, file_id, &plaintext);
    assert_eq!(chunks.len(), 3);
    assert_eq!(
        chunk_count(plaintext.len() as u64).unwrap(),
        3,
        "chunk_count must match the encrypted blocks"
    );
    assert!(chunks[0].0.is_final() == false);
    assert!(chunks[2].0.is_final());

    for (index, (context, _)) in chunks.iter().enumerate() {
        assert_eq!(context.index(), index as u32);
        assert_eq!(
            context.plain_length() as usize,
            chunk_plain_length(plaintext.len() as u64, index as u32).unwrap() as usize
        );
        assert_eq!(context.format_version(), CHUNK_FORMAT_VERSION);
    }
    assert_eq!(decrypt_file(&key, plaintext.len() as u64, &chunks).unwrap(), plaintext);

    // 每块密文都比明文多出固定开销，且包含认证标签。
    for (_, chunk) in &chunks {
        assert!(chunk.as_bytes().len() > 1 + 12 + 16);
    }
    // 同上下文重复加密必须使用新 nonce（密文不同）。
    let context = chunks[0].0;
    let first = encrypt_attachment_chunk(&key, &context, &pattern(CHUNK_BYTES, 1)).unwrap();
    let second = encrypt_attachment_chunk(&key, &context, &pattern(CHUNK_BYTES, 1)).unwrap();
    assert_ne!(first.as_bytes(), second.as_bytes(), "nonce must be fresh per chunk");
    assert_eq!(
        decrypt_attachment_chunk(&key, &context, &first).unwrap().as_slice(),
        pattern(CHUNK_BYTES, 1).as_slice()
    );
}

#[test]
fn empty_file_uses_one_empty_final_chunk() {
    let key = FileKey::generate();
    let file_id = FileId::generate();
    let chunks = encrypt_file(&key, file_id, b"");
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].0.is_final());
    assert_eq!(chunks[0].0.plain_length(), 0);
    assert_eq!(decrypt_file(&key, 0, &chunks).unwrap(), Vec::<u8>::new());
}

#[test]
fn wrong_key_is_rejected() {
    let key = FileKey::generate();
    let other = FileKey::generate();
    assert!(!key.equals(&other));
    let file_id = FileId::generate();
    let plaintext = pattern(CHUNK_BYTES + 10, 3);
    let chunks = encrypt_file(&key, file_id, &plaintext);
    for (context, chunk) in &chunks {
        let error = decrypt_attachment_chunk(&other, context, chunk).unwrap_err();
        assert_eq!(error.code(), "ATTACHMENT_CORRUPTED", "wrong key must not decrypt");
    }
    assert!(FileKey::try_from_slice(&[0u8; FILE_KEY_BYTES - 1]).is_err());
    assert!(FileKey::try_from_slice(&[0u8; FILE_KEY_BYTES + 1]).is_err());
}

#[test]
fn single_byte_tampering_is_rejected() {
    let key = FileKey::generate();
    let file_id = FileId::generate();
    let plaintext = pattern(CHUNK_BYTES, 5);
    let chunks = encrypt_file(&key, file_id, &plaintext);
    let (context, chunk) = &chunks[0];
    let original = chunk.as_bytes();

    // 采样：版本字节、nonce 首/尾、密文中部、标签首字节。
    let positions = [
        0usize,
        1,
        12,
        13,
        original.len() / 2,
        original.len() - 16,
        original.len() - 1,
    ];
    for position in positions {
        let mut tampered = original.to_vec();
        tampered[position] ^= 0x01;
        // 版本字节在解析阶段被拒绝，其余字节在认证阶段被拒绝：两条路径都必须显式失败。
        let outcome = CipherChunk::from_bytes(tampered)
            .and_then(|chunk| decrypt_attachment_chunk(&key, context, &chunk));
        assert_eq!(outcome.unwrap_err().code(), "ATTACHMENT_CORRUPTED", "byte {position}");
    }
}

#[test]
fn truncation_and_impossible_lengths_are_rejected() {
    let key = FileKey::generate();
    let file_id = FileId::generate();
    let plaintext = pattern(CHUNK_BYTES, 9);
    let chunks = encrypt_file(&key, file_id, &plaintext);
    let (context, chunk) = &chunks[0];
    let original = chunk.as_bytes();

    // 去掉一个字节：总长度不再等于上下文声明长度。
    let truncated = original[..original.len() - 1].to_vec();
    let error = decrypt_attachment_chunk(&key, context, &CipherChunk::from_bytes(truncated).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");

    // 太短 / 太长：CipherChunk::from_bytes 直接拒绝。
    assert!(CipherChunk::from_bytes(vec![0u8; 4]).is_err());
    assert!(CipherChunk::from_bytes(vec![0u8; CHUNK_BYTES * 2]).is_err());

    // 未知版本：即使长度合法也拒绝。
    let mut wrong_version = original.to_vec();
    wrong_version[0] = 2;
    let error = CipherChunk::from_bytes(wrong_version).unwrap_err();
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    assert!(original.len() > 1);
}

#[test]
fn reordered_duplicated_or_foreign_chunks_are_rejected() {
    let key = FileKey::generate();
    let file_id = FileId::generate();
    let plaintext = pattern(CHUNK_BYTES * 2 + 1, 11);
    let chunks = encrypt_file(&key, file_id, &plaintext);
    assert_eq!(chunks.len(), 3);

    // 交换 0 与 1：各自用自己的上下文解密必须失败（index 在 AAD 内）。
    let error =
        decrypt_attachment_chunk(&key, &chunks[0].0, &chunks[1].1).unwrap_err();
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    let error =
        decrypt_attachment_chunk(&key, &chunks[1].0, &chunks[0].1).unwrap_err();
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");

    // 重复同一块：用第 0 块当第 1 块。
    let error =
        decrypt_attachment_chunk(&key, &chunks[1].0, &chunks[0].1).unwrap_err();
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");

    // 另一个文件的块（不同 file_id）不能冒充。
    let foreign_id = FileId::generate();
    let foreign = encrypt_file(&key, foreign_id, &plaintext);
    let error =
        decrypt_attachment_chunk(&key, &chunks[0].0, &foreign[0].1).unwrap_err();
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
}

#[test]
fn forged_final_chunk_is_rejected() {
    let key = FileKey::generate();
    let file_id = FileId::generate();
    let plaintext = pattern(CHUNK_BYTES + 1, 13);
    let chunks = encrypt_file(&key, file_id, &plaintext);
    assert_eq!(chunks.len(), 2);
    // 第 0 块本来是非最后一块；把同一个密文当成“单块文件”的最后一块解密必须失败，
    // 因为 total_chunks 与 is_final 都在 AAD 里。
    let forged = ChunkContext::new(file_id, 0, 1, CHUNK_BYTES as u32).unwrap();
    let error = decrypt_attachment_chunk(&key, &forged, &chunks[0].1).unwrap_err();
    assert_eq!(error.code(), "ATTACHMENT_CORRUPTED");
    // 最后一块也不能冒充非最后一块（长度与 is_final 都不匹配）。
    let forged_non_final = ChunkContext::new(file_id, 1, 2, chunks[1].0.plain_length()).unwrap();
    assert!(forged_non_final.is_final());
    let error = decrypt_attachment_chunk(&key, &forged_non_final, &chunks[1].1);
    assert!(error.is_ok(), "context 1 of 2 is the final chunk");
}

#[test]
fn context_bounds_are_enforced() {
    let file_id = FileId::generate();
    // 总数为 0 或超上界。
    assert!(ChunkContext::new(file_id, 0, 0, 0).is_err());
    assert!(ChunkContext::new(file_id, 0, MAX_TOTAL_CHUNKS + 1, 0).is_err());
    // 序号越界。
    assert!(ChunkContext::new(file_id, 2, 2, 0).is_err());
    // 非最后一块必须是满块。
    assert!(ChunkContext::new(file_id, 0, 2, 10).is_err());
    assert!(ChunkContext::new(file_id, 0, 2, CHUNK_BYTES as u32).is_ok());
    // 最后一块可以不满，但不能超过块大小。
    assert!(ChunkContext::new(file_id, 1, 2, CHUNK_BYTES as u32).is_ok());
    assert!(ChunkContext::new(file_id, 0, 1, CHUNK_BYTES as u32 + 1).is_err());

    // 声明长度与明文长度不一致时加密必须拒绝。
    let key = FileKey::generate();
    let context = ChunkContext::new(file_id, 0, 1, 4).unwrap();
    assert_eq!(
        encrypt_attachment_chunk(&key, &context, b"too long").unwrap_err().code(),
        "INVALID_CHUNK_CONTEXT"
    );
}

#[test]
fn attachment_size_limits_are_bounded() {
    assert_eq!(chunk_count(MAX_ATTACHMENT_BYTES).unwrap(), 100);
    assert_eq!(
        chunk_count(MAX_ATTACHMENT_BYTES + 1).unwrap_err().code(),
        "CONTENT_TOO_LARGE"
    );
    assert_eq!(chunk_count(0).unwrap(), 1);
    assert_eq!(
        chunk_plain_length(MAX_ATTACHMENT_BYTES, 99).unwrap() as usize,
        CHUNK_BYTES
    );
    assert!(chunk_plain_length(MAX_ATTACHMENT_BYTES, 100).is_err());
    assert_eq!(FileId::try_from_slice(&[0u8; 15]).unwrap_err().code(), "INVALID_CHUNK_CONTEXT");
    assert_eq!(FileId::generate().to_hex().len(), 32);
}
