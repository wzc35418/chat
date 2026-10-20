//! PLUGINS：插件包（ZIP/JAR）结构校验（**不执行任何插件代码**、不依赖第三方库）。
//!
//! 只读 ZIP 的 **central directory** 与必要的 local header：
//!
//! - 校验条目名（非空、≤512 字节、无 `..` 段、无反斜杠、无控制字符、无重名）；
//! - 校验规模：包 ≤ [MAX_PACKAGE_BYTES]、解压总量 ≤ [MAX_UNCOMPRESSED_BYTES]、条目 ≤ [MAX_ENTRY_COUNT]；
//! - `manifest.json` 必须存在、是合法 UTF-8、且能通过 [`crate::parse_manifest`] 的清单校验；
//! - 清单声明的 `entry_class` 必须有对应的 `com/example/Plugin.class` 条目；
//! - SPI 描述符 `META-INF/services/<entry_class>` 必须存在，且声明的实现名与 `entry_class` 一致。
//!
//! 解压限制：当前只支持 **stored（method=0）** 的条目内容读取（清单/SPI 必须是 stored）；
//! 其它条目可以是任意已广泛支持的压缩方法（我们只看尺寸，不读内容）。真正的 inflate
//! 与代码加载属于后续宿主阶段（见 docs/implementation/plugins.md 限制）。

use std::collections::BTreeSet;

use crate::error::{PluginError, Result};
use crate::limits::PluginLimits;
use crate::manifest::{PluginManifest, parse_manifest};

/// 插件包字节上限（8 MiB）。
pub const MAX_PACKAGE_BYTES: usize = 8 * 1024 * 1024;
/// 解压后总量上限（64 MiB）。
pub const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
/// 条目数上限。
pub const MAX_ENTRY_COUNT: u32 = 4096;
/// 单个条目名上限（字节）。
pub const MAX_ENTRY_NAME_BYTES: usize = 512;
/// 清单文件名（包根）。
pub const MANIFEST_FILE: &str = "manifest.json";
/// SPI 描述符目录（Java 约定）。
pub const SPI_DIRECTORY: &str = "META-INF/services/";

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const EOCD_MIN_BYTES: usize = 22;
const CENTRAL_HEADER_BYTES: usize = 46;
const LOCAL_HEADER_BYTES: usize = 30;
const METHOD_STORED: u16 = 0;
const MAX_COMMENT_SCAN: usize = 0xffff;

/// 包内一个条目的**公开**摘要（不含内容）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageEntryView {
    /// 条目路径（POSIX 风格）。
    pub name: String,
    /// 压缩方法（0 = stored）。
    pub method: u16,
    /// 压缩字节数。
    pub compressed_size: u64,
    /// 解压字节数。
    pub uncompressed_size: u64,
}

/// 插件包结构校验结果（纯数据，不含文件内容）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageInspection {
    /// 清单插件 ID。
    pub plugin_id: String,
    /// 清单版本（`MAJOR.MINOR.PATCH`）。
    pub plugin_version: String,
    /// 清单声明的入口类名。
    pub entry_class: String,
    /// 命中的 SPI 描述符路径（`META-INF/services/<entry_class>`）。
    pub spi_file: String,
    /// 入口类的 `.class` 条目路径。
    pub entry_file: String,
    /// 条目总数。
    pub entry_count: u32,
    /// 全部条目的压缩字节数合计。
    pub compressed_bytes: u64,
    /// 全部条目的解压字节数合计。
    pub uncompressed_bytes: u64,
    /// 清单声明的权限（原样带出；与运行时校验同一套规则，空 = 无敏感权限）。
    pub permissions: Vec<String>,
}

/// 解析出来的一条 central-directory 记录（内部形态）。
struct CentralEntry {
    name: String,
    method: u16,
    compressed_size: u64,
    uncompressed_size: u64,
    local_offset: usize,
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or(PluginError::PackageCorrupted {
            detail: "truncated 16-bit field",
        })?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or(PluginError::PackageCorrupted {
            detail: "truncated 32-bit field",
        })?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// 在尾部找 EOCD（允许最后有 ≤64KiB 的 comment）。
fn find_eocd(bytes: &[u8]) -> Result<usize> {
    if bytes.len() < EOCD_MIN_BYTES {
        return Err(PluginError::PackageCorrupted {
            detail: "package is shorter than an empty zip",
        });
    }
    let scan_from = bytes.len().saturating_sub(EOCD_MIN_BYTES + MAX_COMMENT_SCAN);
    let mut candidate = bytes.len() - EOCD_MIN_BYTES;
    loop {
        if u32_at(bytes, candidate)? == EOCD_SIGNATURE {
            // EOCD 里记录的 comment 长度必须刚好把文件收尾（否则只是正文里的假签名）。
            let comment_len = usize::from(u16_at(bytes, candidate + 20)?);
            if candidate + EOCD_MIN_BYTES + comment_len == bytes.len() {
                return Ok(candidate);
            }
        }
        if candidate == scan_from {
            return Err(PluginError::PackageCorrupted {
                detail: "zip end-of-central-directory record is missing",
            });
        }
        candidate -= 1;
    }
}

/// 校验条目名：可展示、无路径穿越、无重名（重名由调用方集合查重）。
fn validate_entry_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(PluginError::PackageUnsafePath {
            detail: "empty entry name",
        });
    }
    if name.len() > MAX_ENTRY_NAME_BYTES {
        return Err(PluginError::PackageUnsafePath {
            detail: "entry name is too long",
        });
    }
    if name.starts_with('/') || name.contains('\\') {
        return Err(PluginError::PackageUnsafePath {
            detail: "absolute path or backslash in entry name",
        });
    }
    if name.chars().any(char::is_control) {
        return Err(PluginError::PackageUnsafePath {
            detail: "control character in entry name",
        });
    }
    if name.split('/').any(|segment| segment == "..") || name.contains(':') {
        return Err(PluginError::PackageUnsafePath {
            detail: "path traversal segment in entry name",
        });
    }
    Ok(())
}

/// 读取 stored 条目内容（method=0 才支持）。
fn stored_content<'a>(bytes: &'a [u8], entry: &CentralEntry) -> Result<&'a [u8]> {
    if entry.method != METHOD_STORED {
        return Err(PluginError::PackageUnsupportedCompression {
            detail: "the manifest and service descriptor must be stored (method 0)",
        });
    }
    let local = entry.local_offset;
    if u32_at(bytes, local)? != LOCAL_SIGNATURE {
        return Err(PluginError::PackageCorrupted {
            detail: "local file header signature is missing",
        });
    }
    let name_len = usize::from(u16_at(bytes, local + 26)?);
    let extra_len = usize::from(u16_at(bytes, local + 28)?);
    let data_start = local + LOCAL_HEADER_BYTES + name_len + extra_len;
    let data_end = data_start
        .checked_add(usize::try_from(entry.compressed_size).map_err(|_| {
            PluginError::PackageTooLarge {
                detail: "entry is larger than the addressable size",
            }
        })?)
        .ok_or(PluginError::PackageCorrupted {
            detail: "entry data range overflows",
        })?;
    bytes
        .get(data_start..data_end)
        .ok_or(PluginError::PackageCorrupted {
            detail: "entry data range is truncated",
        })
}

fn read_text_entry<'a>(bytes: &'a [u8], entry: &CentralEntry) -> Result<&'a str> {
    let content = stored_content(bytes, entry)?;
    std::str::from_utf8(content).map_err(|_| PluginError::PackageNotText {
        detail: "package entry is not valid UTF-8",
    })
}

/// 解析 central directory（不含内容）。
fn parse_central_directory(bytes: &[u8]) -> Result<Vec<CentralEntry>> {
    let eocd = find_eocd(bytes)?;
    let entry_count = u32::from(u16_at(bytes, eocd + 10)?);
    if entry_count > MAX_ENTRY_COUNT {
        return Err(PluginError::PackageTooLarge {
            detail: "too many zip entries",
        });
    }
    let directory_size = usize::try_from(u32_at(bytes, eocd + 12)?).map_err(|_| {
        PluginError::PackageTooLarge {
            detail: "central directory is larger than the addressable size",
        }
    })?;
    let directory_offset = usize::try_from(u32_at(bytes, eocd + 16)?).map_err(|_| {
        PluginError::PackageTooLarge {
            detail: "central directory offset is out of range",
        }
    })?;
    if directory_offset > eocd || directory_offset.saturating_add(directory_size) > eocd {
        return Err(PluginError::PackageCorrupted {
            detail: "central directory range is out of bounds",
        });
    }
    let mut cursor = directory_offset;
    let mut entries = Vec::with_capacity(entry_count as usize);
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut compressed_total: u64 = 0;
    let mut uncompressed_total: u64 = 0;
    for _ in 0..entry_count {
        if u32_at(bytes, cursor)? != CENTRAL_SIGNATURE {
            return Err(PluginError::PackageCorrupted {
                detail: "central directory record signature is missing",
            });
        }
        let method = u16_at(bytes, cursor + 10)?;
        let compressed = u64::from(u32_at(bytes, cursor + 20)?);
        let uncompressed = u64::from(u32_at(bytes, cursor + 24)?);
        let name_len = usize::from(u16_at(bytes, cursor + 28)?);
        let extra_len = usize::from(u16_at(bytes, cursor + 30)?);
        let comment_len = usize::from(u16_at(bytes, cursor + 32)?);
        let local_offset = usize::try_from(u32_at(bytes, cursor + 42)?).map_err(|_| {
            PluginError::PackageTooLarge {
                detail: "entry offset is out of range",
            }
        })?;
        let name_start = cursor + CENTRAL_HEADER_BYTES;
        let name_bytes = bytes
            .get(name_start..name_start + name_len)
            .ok_or(PluginError::PackageCorrupted {
                detail: "entry name is truncated",
            })?;
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| PluginError::PackageNotText {
                detail: "entry name is not valid UTF-8",
            })?
            .to_owned();
        validate_entry_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(PluginError::PackageDuplicateEntry {
                detail: "the same entry name appears twice",
            });
        }
        compressed_total = compressed_total.checked_add(compressed).ok_or(
            PluginError::PackageTooLarge {
                detail: "compressed size overflows",
            },
        )?;
        uncompressed_total = uncompressed_total.checked_add(uncompressed).ok_or(
            PluginError::PackageTooLarge {
                detail: "uncompressed size overflows",
            },
        )?;
        if uncompressed_total > MAX_UNCOMPRESSED_BYTES {
            return Err(PluginError::PackageTooLarge {
                detail: "uncompressed payload exceeds 64 MiB",
            });
        }
        entries.push(CentralEntry {
            name,
            method,
            compressed_size: compressed,
            uncompressed_size: uncompressed,
            local_offset,
        });
        cursor = name_start + name_len + extra_len + comment_len;
    }
    if compressed_total > MAX_PACKAGE_BYTES as u64 {
        return Err(PluginError::PackageTooLarge {
            detail: "compressed payload exceeds 8 MiB",
        });
    }
    Ok(entries)
}

/// SPI 描述符里的实现名（去注释/空行）必须全部等于入口类。
fn validate_spi(text: &str, entry_class: &str) -> Result<()> {
    let mut declared = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        declared.push(trimmed);
    }
    if declared.is_empty() {
        return Err(PluginError::PackageSpiMismatch {
            detail: "service descriptor declares no implementation",
        });
    }
    if declared.iter().any(|name| *name != entry_class) {
        return Err(PluginError::PackageSpiMismatch {
            detail: "service descriptor implementation does not match the entry class",
        });
    }
    Ok(())
}

/// 入口类的 `.class` 条目路径（`com.example.Plugin` → `com/example/Plugin.class`）。
fn entry_file_for(entry_class: &str) -> String {
    format!("{}.class", entry_class.replace('.', "/"))
}

/// 校验插件包结构（读取字节，不执行任何代码）。
pub fn inspect_plugin_package(bytes: &[u8], limits: &PluginLimits) -> Result<PackageInspection> {
    limits.validate()?;
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(PluginError::PackageTooLarge {
            detail: "package exceeds 8 MiB",
        });
    }
    let entries = parse_central_directory(bytes)?;
    let manifest_entry = entries
        .iter()
        .find(|entry| entry.name == MANIFEST_FILE)
        .ok_or(PluginError::PackageMissingManifest)?;
    let manifest_text = read_text_entry(bytes, manifest_entry)?;
    let manifest: PluginManifest = parse_manifest(manifest_text, limits)?;

    // 入口类必须有对应的 .class 条目。
    let entry_file = entry_file_for(&manifest.entry_class);
    if !entries.iter().any(|entry| entry.name == entry_file) {
        return Err(PluginError::PackageMissingEntry);
    }

    // SPI 描述符必须存在，且声明的实现名 == entryClass。
    let spi_file = format!("{SPI_DIRECTORY}{}", manifest.entry_class);
    let spi_entry = entries
        .iter()
        .find(|entry| entry.name == spi_file)
        .ok_or(PluginError::PackageSpiMismatch {
            detail: "the package has no META-INF/services descriptor for the entry class",
        })?;
    let spi_text = read_text_entry(bytes, spi_entry)?;
    validate_spi(spi_text, &manifest.entry_class)?;

    let compressed_bytes = entries
        .iter()
        .map(|entry| entry.compressed_size)
        .sum::<u64>();
    let uncompressed_bytes = entries
        .iter()
        .map(|entry| entry.uncompressed_size)
        .sum::<u64>();
    Ok(PackageInspection {
        plugin_id: manifest.id.as_str().to_owned(),
        plugin_version: format!(
            "{}.{}.{}",
            manifest.version.major, manifest.version.minor, manifest.version.patch
        ),
        entry_class: manifest.entry_class.clone(),
        spi_file,
        entry_file,
        entry_count: u32::try_from(entries.len()).unwrap_or(u32::MAX),
        compressed_bytes,
        uncompressed_bytes,
        permissions: manifest
            .permissions
            .iter()
            .map(|permission| permission.as_str().to_owned())
            .collect(),
    })
}

/// 取出包根 `manifest.json` 的文本内容（stored 条目；安装事务据此拿强类型清单）。
pub fn package_manifest_json(bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(PluginError::PackageTooLarge {
            detail: "package exceeds 8 MiB",
        });
    }
    let entries = parse_central_directory(bytes)?;
    let manifest_entry = entries
        .iter()
        .find(|entry| entry.name == MANIFEST_FILE)
        .ok_or(PluginError::PackageMissingManifest)?;
    Ok(read_text_entry(bytes, manifest_entry)?.to_owned())
}

/// 包内条目摘要（诊断/预览用；只读 central directory，不读内容）。
pub fn list_package_entries(bytes: &[u8]) -> Result<Vec<PackageEntryView>> {
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(PluginError::PackageTooLarge {
            detail: "package exceeds 8 MiB",
        });
    }
    Ok(parse_central_directory(bytes)?
        .into_iter()
        .map(|entry| PackageEntryView {
            name: entry.name,
            method: entry.method,
            compressed_size: entry.compressed_size,
            uncompressed_size: entry.uncompressed_size,
        })
        .collect())
}
