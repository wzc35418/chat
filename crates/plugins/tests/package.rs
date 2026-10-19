//! PLUGINS：插件包（ZIP）结构校验用例（全部用测试内构造的最小 ZIP 字节）。
//!
//! 覆盖：合法包通过；缺清单 / 缺入口类 / SPI 不符 / 路径穿越 / 重名 / 超量 / 超条目 /
//! 压缩方法不支持 / ZIP 损坏 / 清单非法 各自显式拒绝。

use chat_plugins::package::{
    MANIFEST_FILE, MAX_ENTRY_COUNT, MAX_PACKAGE_BYTES, MAX_UNCOMPRESSED_BYTES,
    inspect_plugin_package,
};
use chat_plugins::{PluginLimits, PluginManifest, parse_manifest};

const ENTRY_CLASS: &str = "com.example.Plugin";
const SPI_FILE: &str = "META-INF/services/com.example.Plugin";
const CLASS_FILE: &str = "com/example/Plugin.class";

fn manifest_json(entry_class: &str) -> String {
    format!(
        "{{\"schemaVersion\":1,\"id\":\"notes.v1\",\"version\":\"1.2.3\",\"sdkVersion\":1,\
         \"platform\":\"desktop\",\"capabilities\":[\"Command\"],\"permissions\":[],\
         \"extensionPoints\":[\"Command\"],\"entryClass\":\"{entry_class}\"}}"
    )
}

/// 最小 ZIP 构造器：stored 条目、无 extra/comment、CRC 写 0（本层不校验 CRC：
/// 不读内容的压缩版本无法验 CRC，已在文档写明）。
struct ZipBuilder {
    entries: Vec<(String, Vec<u8>, u16)>,
}

impl ZipBuilder {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// 追加一个 stored 条目。
    fn stored(mut self, name: &str, data: &[u8]) -> Self {
        self.entries.push((name.to_owned(), data.to_vec(), 0));
        self
    }

    /// 追加一个声明了压缩方法但内容不压缩的条目（用于测"不支持压缩方法"）。
    fn with_method(mut self, name: &str, data: &[u8], method: u16) -> Self {
        self.entries.push((name.to_owned(), data.to_vec(), method));
        self
    }

    fn build(self) -> Vec<u8> {
        let sizes: Vec<(u64, u64)> = self
            .entries
            .iter()
            .map(|(_, data, _)| (data.len() as u64, data.len() as u64))
            .collect();
        self.build_with_sizes(&sizes)
    }

    fn build_with_sizes(self, sizes: &[(u64, u64)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (index, (name, data, method)) in self.entries.iter().enumerate() {
            let (declared_compressed, declared_uncompressed) = sizes
                .get(index)
                .copied()
                .unwrap_or((data.len() as u64, data.len() as u64));
            let local_offset = out.len() as u32;
            out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
            out.extend_from_slice(&20_u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0_u16.to_le_bytes()); // flags
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes()); // time
            out.extend_from_slice(&0_u16.to_le_bytes()); // date
            out.extend_from_slice(&0_u32.to_le_bytes()); // crc
            out.extend_from_slice(&(declared_compressed as u32).to_le_bytes());
            out.extend_from_slice(&(declared_uncompressed as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0_u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
            central.extend_from_slice(&20_u16.to_le_bytes()); // version made
            central.extend_from_slice(&20_u16.to_le_bytes()); // version needed
            central.extend_from_slice(&0_u16.to_le_bytes()); // flags
            central.extend_from_slice(&method.to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes());
            central.extend_from_slice(&0_u32.to_le_bytes()); // crc
            central.extend_from_slice(&(declared_compressed as u32).to_le_bytes());
            central.extend_from_slice(&(declared_uncompressed as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&0_u16.to_le_bytes()); // extra
            central.extend_from_slice(&0_u16.to_le_bytes()); // comment
            central.extend_from_slice(&0_u16.to_le_bytes()); // disk
            central.extend_from_slice(&0_u16.to_le_bytes()); // internal attrs
            central.extend_from_slice(&0_u32.to_le_bytes()); // external attrs
            central.extend_from_slice(&local_offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let central_offset = out.len() as u32;
        let central_size = central.len() as u32;
        out.extend_from_slice(&central);
        let count = self.entries.len() as u16;
        out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&central_size.to_le_bytes());
        out.extend_from_slice(&central_offset.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes());
        out
    }
}

fn valid_package() -> Vec<u8> {
    ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .stored(CLASS_FILE, b"\xca\xfe\xba\xbe")
        .stored(SPI_FILE, ENTRY_CLASS.as_bytes())
        .build()
}

fn code_of(bytes: &[u8]) -> String {
    let limits = PluginLimits::default();
    inspect_plugin_package(bytes, &limits)
        .expect_err("inspection must fail")
        .code()
        .to_owned()
}

#[test]
fn a_valid_package_is_inspected_without_executing_anything() {
    let bytes = valid_package();
    let limits = PluginLimits::default();
    let view = inspect_plugin_package(&bytes, &limits).expect("valid package");
    assert_eq!(view.plugin_id, "notes.v1");
    assert_eq!(view.plugin_version, "1.2.3");
    assert_eq!(view.entry_class, ENTRY_CLASS);
    assert_eq!(view.entry_file, CLASS_FILE);
    assert_eq!(view.spi_file, SPI_FILE);
    assert_eq!(view.entry_count, 3);
    assert!(view.compressed_bytes > 0);
    assert_eq!(view.compressed_bytes, view.uncompressed_bytes, "stored entries");
    // 清单也能单独解析（同一份规则）。
    let manifest: PluginManifest =
        parse_manifest(&manifest_json(ENTRY_CLASS), &limits).expect("manifest parses");
    assert_eq!(manifest.entry_class, ENTRY_CLASS);
}

#[test]
fn a_package_without_a_manifest_is_rejected() {
    let bytes = ZipBuilder::new()
        .stored(CLASS_FILE, b"\xca\xfe\xba\xbe")
        .stored(SPI_FILE, ENTRY_CLASS.as_bytes())
        .build();
    assert_eq!(code_of(&bytes), "PLUGIN_PACKAGE_MISSING_MANIFEST");
}

#[test]
fn a_package_without_the_entry_class_is_rejected() {
    let bytes = ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .stored(SPI_FILE, ENTRY_CLASS.as_bytes())
        .build();
    assert_eq!(code_of(&bytes), "PLUGIN_PACKAGE_MISSING_ENTRY");
}

#[test]
fn a_service_descriptor_for_another_implementation_is_rejected() {
    let bytes = ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .stored(CLASS_FILE, b"\xca\xfe\xba\xbe")
        .stored(SPI_FILE, b"com.example.Other")
        .build();
    assert_eq!(code_of(&bytes), "PLUGIN_PACKAGE_SPI_MISMATCH");
}

#[test]
fn a_missing_service_descriptor_is_rejected() {
    let bytes = ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .stored(CLASS_FILE, b"\xca\xfe\xba\xbe")
        .build();
    assert_eq!(code_of(&bytes), "PLUGIN_PACKAGE_SPI_MISMATCH");
}

#[test]
fn path_traversal_and_duplicate_entries_are_rejected() {
    let traversal = ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .stored(CLASS_FILE, b"\xca\xfe\xba\xbe")
        .stored(SPI_FILE, ENTRY_CLASS.as_bytes())
        .stored("../evil.class", b"x")
        .build();
    assert_eq!(code_of(&traversal), "PLUGIN_PACKAGE_UNSAFE_PATH");

    let absolute = ZipBuilder::new()
        .stored("/etc/passwd", b"x")
        .build();
    assert_eq!(code_of(&absolute), "PLUGIN_PACKAGE_UNSAFE_PATH");

    let duplicate = ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .build();
    assert_eq!(code_of(&duplicate), "PLUGIN_PACKAGE_DUPLICATE_ENTRY");
}

#[test]
fn oversized_packages_are_rejected_before_parsing() {
    // 输入 > 8 MiB。
    let too_big = vec![0_u8; MAX_PACKAGE_BYTES + 1];
    assert_eq!(code_of(&too_big), "PLUGIN_PACKAGE_TOO_LARGE");

    // 条目数 > 4096。
    let mut builder = ZipBuilder::new();
    for index in 0..=MAX_ENTRY_COUNT {
        builder = builder.stored(&format!("f{index}.bin"), b"");
    }
    let many = builder.build();
    assert_eq!(code_of(&many), "PLUGIN_PACKAGE_TOO_LARGE");

    // 声明解压总量 > 64 MiB（不提供真实数据：解析只看 central directory 的声明尺寸）。
    let inflated = ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .build_with_sizes(&[
            (
                manifest_json(ENTRY_CLASS).len() as u64,
                MAX_UNCOMPRESSED_BYTES + 1,
            ),
        ]);
    assert_eq!(code_of(&inflated), "PLUGIN_PACKAGE_TOO_LARGE");
}

#[test]
fn unsupported_compression_for_the_manifest_is_rejected() {
    let bytes = ZipBuilder::new()
        .with_method(MANIFEST_FILE, b"compressed-bytes", 8)
        .stored(CLASS_FILE, b"\xca\xfe\xba\xbe")
        .stored(SPI_FILE, ENTRY_CLASS.as_bytes())
        .build();
    assert_eq!(code_of(&bytes), "PLUGIN_PACKAGE_UNSUPPORTED_COMPRESSION");
}

#[test]
fn corrupted_zips_and_bad_manifests_are_rejected() {
    // 完全不是 ZIP。
    assert_eq!(code_of(b"not a zip at all"), "PLUGIN_PACKAGE_CORRUPTED");
    // 截断的 EOCD。
    let mut truncated = valid_package();
    truncated.truncate(truncated.len() - 5);
    assert_eq!(code_of(&truncated), "PLUGIN_PACKAGE_CORRUPTED");
    // 清单不是合法 JSON（走清单层稳定码）。
    let bad_manifest = ZipBuilder::new()
        .stored(MANIFEST_FILE, b"{}")
        .stored(CLASS_FILE, b"x")
        .stored(SPI_FILE, ENTRY_CLASS.as_bytes())
        .build();
    assert_eq!(code_of(&bad_manifest), "INVALID_PLUGIN_PACKAGE");
    // 清单不是 UTF-8。
    let not_text = ZipBuilder::new()
        .stored(MANIFEST_FILE, &[0xff, 0xfe])
        .stored(CLASS_FILE, b"x")
        .stored(SPI_FILE, ENTRY_CLASS.as_bytes())
        .build();
    assert_eq!(code_of(&not_text), "PLUGIN_PACKAGE_NOT_TEXT");
}

#[test]
fn entry_names_are_bounded_and_reject_colons() {
    let long_name = format!("a{}.class", "b".repeat(600));
    let bytes = ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .stored(&long_name, b"x")
        .build();
    assert_eq!(code_of(&bytes), "PLUGIN_PACKAGE_UNSAFE_PATH");
    let colon = ZipBuilder::new()
        .stored(MANIFEST_FILE, manifest_json(ENTRY_CLASS).as_bytes())
        .stored("C:/evil.class", b"x")
        .build();
    assert_eq!(code_of(&colon), "PLUGIN_PACKAGE_UNSAFE_PATH");
}
