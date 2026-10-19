//! 附件 metadata（协议字段 57）承载展示名：跨设备一致、版本化兼容、边界与清洗。
//!
//! 覆盖：中文名跨设备一致（真双设备两库 + exporter 密钥）；128 字节接受、129 字节显式拒绝；
//! 老格式（v1，无名字）解析回退占位名；ASCII 控制字符被过滤、空名回退占位。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::attachments::{
    ATTACHMENT_METADATA_DISPLAY_NAME, ATTACHMENT_METADATA_VERSION,
    ATTACHMENT_METADATA_VERSION_DISPLAY_NAME, MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES,
    MAX_ATTACHMENT_METADATA_BYTES, PersistedAttachmentStore, attachment_exporter_wrapping_key,
    encode_attachment_metadata, metadata_display_name_for_encode, parse_attachment_metadata,
    parse_attachment_message_body, sanitize_metadata_display_name,
};
use chat_core::groups::{CreateGroupRequest, GroupBootstrapPeer, create_group};
use chat_crypto::engine::MlsDevice;
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, StoreOptions, open_store};

const MASTER_KEY: [u8; 32] = [0x5a; 32];
const PLAINTEXT: &[u8] = b"display name attachment plaintext";

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn conversation_text() -> String {
    chat_protocol::ConversationId::from_bytes([0x11; 32]).encode()
}

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-display-name-{tag}-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> PathBuf {
        self.0.join("chat.db")
    }

    fn cache(&self) -> PathBuf {
        self.0.join("attachments")
    }

    fn store(&self) -> chat_storage::formal::Store {
        open_store(
            StoreOptions::new(self.database(), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open store")
    }

    fn attachments(&self) -> PersistedAttachmentStore {
        PersistedAttachmentStore::open(self.cache(), self.store()).expect("open attachment store")
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).expect("write fixture file");
        path
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 两设备同群：alice（库 A）与 bob（库 B）各自持久化同一 epoch 的群状态。
fn mls_pair(dir_a: &FixtureDir, dir_b: &FixtureDir) -> (MlsDevice, String) {
    let mut alice = MlsDevice::new("alice").expect("alice");
    let bob = MlsDevice::new("bob").expect("bob");
    let mut store_a = dir_a.store();
    let request = CreateGroupRequest::new("展示名群", "alice");
    let peer = GroupBootstrapPeer {
        credential: "bob".to_owned(),
        device: &bob,
    };
    let group = create_group(&mut alice, &peer, &mut store_a, &request).expect("create group");
    alice.export_to_store(&mut store_a).expect("persist alice");
    drop(store_a);
    let mut store_b = dir_b.store();
    alice.export_to_store(&mut store_b).expect("persist into receiver store");
    let _ = master_key_hex();
    (alice, group.group_id)
}

/// 在 A 的库里导入一个带展示名的附件，返回 (attachment_id, 字段 57 元数据)。
fn import_and_encode(
    dir_a: &FixtureDir,
    dir_b: &FixtureDir,
    alice: &MlsDevice,
    name: &str,
) -> (String, Vec<u8>) {
    let attachments = dir_a.attachments();
    let source = dir_a.write("source.bin", PLAINTEXT);
    let attachment_id = attachments
        .import_file(&chat_core::attachments::FileToken::from_local_path(&source), name)
        .expect("import attachment")
        .attachment_id;
    // 包装密钥从**另一个独立库**派生（单写者：A 的库此刻被 attachments 持有）。
    let key = {
        let store_b = dir_b.store();
        attachment_exporter_wrapping_key(alice, &store_b, &attachment_id, &conversation_text(), 0)
            .expect("exporter key")
    };
    let body_text = attachments
        .encode_attachment_message_body(&attachment_id, &key)
        .expect("attachment message body");
    let body = parse_attachment_message_body(&body_text)
        .expect("parse body")
        .expect("attachment body");
    let metadata = encode_attachment_metadata(&body).expect("encode metadata");
    (attachment_id, metadata)
}

#[test]
fn chinese_display_name_survives_the_cross_device_metadata_path() {
    let dir_a = FixtureDir::new("a");
    let dir_b = FixtureDir::new("b");
    let (alice, _group_id) = mls_pair(&dir_a, &dir_b);
    let (attachment_id, metadata) = import_and_encode(&dir_a, &dir_b, &alice, "会议纪要.txt");
    assert_eq!(metadata[0], ATTACHMENT_METADATA_VERSION_DISPLAY_NAME);
    assert!(metadata.len() <= MAX_ATTACHMENT_METADATA_BYTES);
    // 编码后的字节里带着名字（名字不进正文、也不参与路径）。
    assert!(
        metadata.windows("会议纪要.txt".len()).any(|window| window == "会议纪要.txt".as_bytes()),
        "the display name must travel inside field 57"
    );
    // 本机解析：名字一致。
    let parsed = parse_attachment_metadata(&metadata).expect("parse metadata");
    assert_eq!(parsed.display_name, "会议纪要.txt");
    // 对端（独立库 B）用 exporter 密钥采用 → 附件记录里的展示名一致。
    let bob_key = {
        let store_b = dir_b.store();
        attachment_exporter_wrapping_key(&alice, &store_b, &attachment_id, &conversation_text(), 0)
            .expect("receiver exporter key")
    };
    let mut attachments_b = dir_b.attachments();
    let adopted = attachments_b
        .adopt_attachment_metadata(&metadata, Some(&bob_key), None)
        .expect("receiver adopts metadata");
    assert_eq!(adopted, attachment_id);
    let record = attachments_b
        .attachment_record(&attachment_id)
        .expect("record")
        .expect("record exists");
    assert_eq!(record.display_name, "会议纪要.txt");
}

#[test]
fn a_name_of_128_bytes_is_accepted_and_129_is_rejected() {
    let dir = FixtureDir::new("bounds");
    let dir_b = FixtureDir::new("bounds-b");
    let (alice, _group_id) = mls_pair(&dir, &dir_b);
    let (_attachment_id, metadata) = import_and_encode(&dir, &dir_b, &alice, "bounds.bin");
    let mut body = parse_attachment_metadata(&metadata).expect("parse metadata");
    // 128 字节（ASCII）恰好接受。
    body.display_name = "x".repeat(MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES);
    let encoded = encode_attachment_metadata(&body).expect("128 bytes accepted");
    assert_eq!(
        parse_attachment_metadata(&encoded).expect("parse").display_name.len(),
        MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES
    );
    // 129 字节显式拒绝（绝不静默截断）。
    body.display_name = "y".repeat(MAX_ATTACHMENT_METADATA_DISPLAY_NAME_BYTES + 1);
    let error = encode_attachment_metadata(&body).expect_err("129 bytes rejected");
    assert_eq!(error.code(), "CONTENT_TOO_LARGE");
    // 清洗规则本身也拒绝超长。
    let error = metadata_display_name_for_encode(&"z".repeat(200)).expect_err("too long");
    assert_eq!(error.code(), "CONTENT_TOO_LARGE");
}

#[test]
fn legacy_metadata_without_a_name_falls_back_to_the_placeholder() {
    let dir = FixtureDir::new("legacy");
    let dir_b = FixtureDir::new("legacy-b");
    let (alice, _group_id) = mls_pair(&dir, &dir_b);
    let (_attachment_id, metadata) = import_and_encode(&dir, &dir_b, &alice, "legacy.bin");
    // 构造 v1 字节：把名字段整段去掉，版本号改回 1。
    let legacy = strip_display_name(&metadata);
    assert_eq!(legacy[0], ATTACHMENT_METADATA_VERSION);
    assert!(legacy.len() < metadata.len());
    let parsed = parse_attachment_metadata(&legacy).expect("v1 metadata still parses");
    assert_eq!(parsed.display_name, ATTACHMENT_METADATA_DISPLAY_NAME);
    assert_eq!(parsed.chunk_count, 1);
}

/// 把 v2 元数据改写成 v1（删掉 `name_len ‖ name` 段，版本号置 1）。
fn strip_display_name(metadata: &[u8]) -> Vec<u8> {
    const HEADER: usize = 1 + 16 + 8 + 8 + 4 + 4 + 32 + 32 + 2;
    let mut cursor = HEADER;
    let descriptor_len = usize::from(u16::from_be_bytes([
        metadata[HEADER - 2],
        metadata[HEADER - 1],
    ]));
    cursor += descriptor_len;
    let name_len = usize::from(metadata[cursor]);
    let mut legacy = Vec::with_capacity(metadata.len() - name_len - 1);
    legacy.extend_from_slice(&metadata[..1]);
    legacy[0] = ATTACHMENT_METADATA_VERSION;
    legacy.extend_from_slice(&metadata[1..cursor]);
    legacy.extend_from_slice(&metadata[cursor + 1 + name_len..]);
    legacy
}

#[test]
fn control_characters_are_filtered_and_blank_names_fall_back() {
    let dir = FixtureDir::new("clean");
    let dir_b = FixtureDir::new("clean-b");
    let (alice, _group_id) = mls_pair(&dir, &dir_b);
    let (_attachment_id, metadata) = import_and_encode(&dir, &dir_b, &alice, "clean.bin");
    let mut body = parse_attachment_metadata(&metadata).expect("parse metadata");
    // 控制字符（\t \n \u{7f}）被过滤，首尾空白被裁剪。
    body.display_name = "  a\tb\nc\u{7f}.txt  ".to_owned();
    let encoded = encode_attachment_metadata(&body).expect("encode");
    assert_eq!(parse_attachment_metadata(&encoded).expect("parse").display_name, "abc.txt");
    // 纯空白 → 回退占位名。
    body.display_name = "   ".to_owned();
    let encoded = encode_attachment_metadata(&body).expect("encode");
    assert_eq!(
        parse_attachment_metadata(&encoded).expect("parse").display_name,
        ATTACHMENT_METADATA_DISPLAY_NAME
    );
    // 清洗函数自身的行为（供上层复用）。
    assert_eq!(sanitize_metadata_display_name(" x\u{1}y "), "xy");
    assert_eq!(sanitize_metadata_display_name(""), ATTACHMENT_METADATA_DISPLAY_NAME);
    // 接收侧写记录时用的就是清洗后的名字。
    let attachment_id = parse_attachment_metadata(&metadata).expect("parse").attachment_id;
    let key = {
        let store = dir.store();
        attachment_exporter_wrapping_key(&alice, &store, &attachment_id, &conversation_text(), 0)
            .expect("key")
    };
    let mut attachments_b = dir_b.attachments();
    body.display_name = " 报告\u{7f}.pdf ".to_owned();
    let clean_metadata = encode_attachment_metadata(&body).expect("encode");
    attachments_b
        .adopt_attachment_metadata(&clean_metadata, Some(&key), None)
        .expect("adopt with a sanitized name");
    let record = attachments_b
        .attachment_record(&attachment_id)
        .expect("record")
        .expect("record exists");
    assert_eq!(record.display_name, "报告.pdf");
}
