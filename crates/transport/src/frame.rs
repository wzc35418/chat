//! TRANSPORT-003 的纯逻辑子集：帧编解码与分片重组。
//!
//! 冻结线格式（每个分片一条记录）：
//!
//! ```text
//! [u32 BE wire_len]            // = 12 + payload_len（长度前缀，不含自身 4 字节）
//! [u32 BE frame_id]            // 帧编号（同一连接内单调递增）
//! [u16 BE fragment_index]      // 0 起
//! [u16 BE fragment_count]      // ≥1
//! [u32 BE total_len]           // 完整帧字节数
//! [payload ...]                // 当前分片字节
//! ```
//!
//! 分片大小固定为 [`FrameCodec`] 配置的 `max_fragment_bytes`（协商结果的一部分），
//! 因此切分是规范且唯一的：`count = ceil(total_len / max_fragment_bytes)`，
//! 非末尾分片长度必须等于 `max_fragment_bytes`。任何非规范切分都在解码期拒绝。
//!
//! 重组（[`FragmentAssembler`]）是严格有序的：分片索引必须从 0 连续递增，
//! 重复分片 `DUPLICATE_FRAGMENT`、乱序 `OUT_OF_ORDER_FRAGMENT`、旧帧 `STALE_FRAME`、
//! 未收齐就换帧 `INCOMPLETE_FRAME`。所有长度先检查后分配，任何长度声明都不会
//! 触发超大内存分配。

use core::fmt;

use crate::error::{Result, TransportError};

/// 长度前缀字节数。
pub const LENGTH_PREFIX_BYTES: usize = 4;
/// 分片元数据字节数（不含长度前缀）。
pub const FRAGMENT_META_BYTES: usize = 12;
/// 分片记录头总字节数。
pub const FRAGMENT_HEADER_BYTES: usize = LENGTH_PREFIX_BYTES + FRAGMENT_META_BYTES;
/// 单帧硬上限（与协议规范编码上限一致）。
pub const FRAME_HARD_MAX_BYTES: usize = chat_protocol::MAX_CANONICAL_BYTES;
/// 分片数硬上限。
pub const MAX_FRAGMENTS_PER_FRAME_HARD: usize = 1024;

/// 帧预算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameLimits {
    /// 完整帧字节上限（1..=64 KiB）。
    pub max_frame_bytes: usize,
    /// 单分片字节上限（1..=max_frame_bytes）。
    pub max_fragment_bytes: usize,
    /// 单帧分片数上限（1..=1024，且必须足够切分 `max_frame_bytes`）。
    pub max_fragments_per_frame: usize,
}

impl FrameLimits {
    /// 默认预算：64 KiB 帧、4 KiB 分片、64 片。
    pub const DEFAULT: FrameLimits = FrameLimits {
        max_frame_bytes: 64 * 1024,
        max_fragment_bytes: 4 * 1024,
        max_fragments_per_frame: 64,
    };

    /// 有界校验；畸形配置显式拒绝，不做“夹到合法值”。
    pub fn validate(&self) -> Result<()> {
        if self.max_frame_bytes == 0 || self.max_frame_bytes > FRAME_HARD_MAX_BYTES {
            return Err(TransportError::InvalidFrameLimits {
                detail: "maxFrameBytes must be within 1..=65536",
            });
        }
        if self.max_fragment_bytes == 0 || self.max_fragment_bytes > self.max_frame_bytes {
            return Err(TransportError::InvalidFrameLimits {
                detail: "maxFragmentBytes must be within 1..=maxFrameBytes",
            });
        }
        if self.max_fragments_per_frame == 0
            || self.max_fragments_per_frame > MAX_FRAGMENTS_PER_FRAME_HARD
        {
            return Err(TransportError::InvalidFrameLimits {
                detail: "maxFragmentsPerFrame must be within 1..=1024",
            });
        }
        let required = self.max_frame_bytes.div_ceil(self.max_fragment_bytes);
        if required > self.max_fragments_per_frame {
            return Err(TransportError::InvalidFrameLimits {
                detail: "maxFragmentsPerFrame cannot cover maxFrameBytes",
            });
        }
        Ok(())
    }
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn expected_fragment_count(total_len: usize, chunk: usize) -> usize {
    total_len.div_ceil(chunk)
}

/// 规范分片布局：第 `index` 片的长度。
fn canonical_fragment_len(total_len: usize, chunk: usize, index: usize, count: usize) -> usize {
    if index + 1 == count {
        total_len - chunk * (count - 1)
    } else {
        chunk
    }
}

/// 一个帧分片（字段私有：只能由编解码器构造）。
#[derive(Clone, PartialEq, Eq)]
pub struct Fragment {
    frame_id: u32,
    index: u16,
    count: u16,
    total_len: u32,
    payload: Vec<u8>,
}

impl Fragment {
    /// 帧编号。
    pub const fn frame_id(&self) -> u32 {
        self.frame_id
    }

    /// 分片索引（0 起）。
    pub const fn index(&self) -> u16 {
        self.index
    }

    /// 分片总数。
    pub const fn count(&self) -> u16 {
        self.count
    }

    /// 完整帧字节数。
    pub const fn total_len(&self) -> u32 {
        self.total_len
    }

    /// 本分片负载。
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// 规范线格式字节（带长度前缀）。
    pub fn wire_bytes(&self) -> Vec<u8> {
        let wire_len = FRAGMENT_META_BYTES + self.payload.len();
        let mut wire = Vec::with_capacity(LENGTH_PREFIX_BYTES + wire_len);
        wire.extend_from_slice(&(wire_len as u32).to_be_bytes());
        wire.extend_from_slice(&self.frame_id.to_be_bytes());
        wire.extend_from_slice(&self.index.to_be_bytes());
        wire.extend_from_slice(&self.count.to_be_bytes());
        wire.extend_from_slice(&self.total_len.to_be_bytes());
        wire.extend_from_slice(&self.payload);
        wire
    }
}

impl fmt::Debug for Fragment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Fragment(frame={}, {}/{}, total={}, payload={}B)",
            self.frame_id,
            self.index,
            self.count,
            self.total_len,
            self.payload.len()
        )
    }
}

/// 帧编解码器。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCodec {
    limits: FrameLimits,
}

impl FrameCodec {
    /// 创建编解码器；预算非法显式拒绝。
    pub fn new(limits: FrameLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self { limits })
    }

    /// 当前预算。
    pub const fn limits(&self) -> FrameLimits {
        self.limits
    }

    /// 把完整帧切分为规范分片。
    pub fn encode(&self, frame_id: u32, frame: &[u8]) -> Result<Vec<Fragment>> {
        if frame.is_empty() {
            return Err(TransportError::MalformedFrame {
                detail: "empty frame",
            });
        }
        if frame.len() > self.limits.max_frame_bytes {
            return Err(TransportError::ContentTooLarge {
                size: frame.len(),
                limit: self.limits.max_frame_bytes,
            });
        }
        let chunk = self.limits.max_fragment_bytes;
        let count = expected_fragment_count(frame.len(), chunk);
        if count > self.limits.max_fragments_per_frame {
            return Err(TransportError::ContentTooLarge {
                size: frame.len(),
                limit: self.limits.max_fragments_per_frame,
            });
        }
        let total_len =
            u32::try_from(frame.len()).map_err(|_| TransportError::ContentTooLarge {
                size: frame.len(),
                limit: self.limits.max_frame_bytes,
            })?;
        let count_u16 = u16::try_from(count).map_err(|_| TransportError::ContentTooLarge {
            size: frame.len(),
            limit: self.limits.max_fragments_per_frame,
        })?;
        let mut fragments = Vec::with_capacity(count);
        for index in 0..count {
            let start = index * chunk;
            let len = canonical_fragment_len(frame.len(), chunk, index, count);
            fragments.push(Fragment {
                frame_id,
                index: index as u16,
                count: count_u16,
                total_len,
                payload: frame[start..start + len].to_vec(),
            });
        }
        Ok(fragments)
    }

    /// 切分并直接产出规范线格式字节（每片一个记录）。
    pub fn encode_wire(&self, frame_id: u32, frame: &[u8]) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .encode(frame_id, frame)?
            .into_iter()
            .map(|fragment| fragment.wire_bytes())
            .collect())
    }

    /// 解码一个分片记录。
    ///
    /// 先检查长度前缀与总长，再解析字段：任何声明都不会触发超大分配。
    pub fn decode_wire(&self, wire: &[u8]) -> Result<Fragment> {
        if wire.len() < LENGTH_PREFIX_BYTES {
            return Err(TransportError::MalformedFrame {
                detail: "truncated length prefix",
            });
        }
        let declared = u32::from_be_bytes([wire[0], wire[1], wire[2], wire[3]]) as usize;
        if declared != wire.len() - LENGTH_PREFIX_BYTES {
            return Err(TransportError::MalformedFrame {
                detail: "length prefix does not match the record size",
            });
        }
        if wire.len() < FRAGMENT_HEADER_BYTES {
            return Err(TransportError::MalformedFrame {
                detail: "truncated fragment header",
            });
        }
        let frame_id = u32::from_be_bytes([wire[4], wire[5], wire[6], wire[7]]);
        let index = u16::from_be_bytes([wire[8], wire[9]]);
        let count = u16::from_be_bytes([wire[10], wire[11]]);
        let total_len = u32::from_be_bytes([wire[12], wire[13], wire[14], wire[15]]) as usize;
        if total_len == 0 {
            return Err(TransportError::MalformedFrame {
                detail: "empty frame",
            });
        }
        if total_len > self.limits.max_frame_bytes {
            return Err(TransportError::ContentTooLarge {
                size: total_len,
                limit: self.limits.max_frame_bytes,
            });
        }
        if count == 0 || count > MAX_FRAGMENTS_PER_FRAME_HARD as u16 {
            return Err(TransportError::MalformedFrame {
                detail: "fragment count out of range",
            });
        }
        let count_usize = count as usize;
        if count_usize > self.limits.max_fragments_per_frame {
            return Err(TransportError::MalformedFrame {
                detail: "fragment count exceeds the configured budget",
            });
        }
        if index >= count {
            return Err(TransportError::MalformedFrame {
                detail: "fragment index out of range",
            });
        }
        let payload = &wire[FRAGMENT_HEADER_BYTES..];
        if payload.len() > self.limits.max_fragment_bytes {
            return Err(TransportError::ContentTooLarge {
                size: payload.len(),
                limit: self.limits.max_fragment_bytes,
            });
        }
        let chunk = self.limits.max_fragment_bytes;
        if expected_fragment_count(total_len, chunk) != count_usize {
            return Err(TransportError::MalformedFrame {
                detail: "fragment count does not match the frame size",
            });
        }
        let expected_len = canonical_fragment_len(total_len, chunk, index as usize, count_usize);
        if payload.len() != expected_len {
            return Err(TransportError::MalformedFrame {
                detail: "fragment size does not match canonical chunking",
            });
        }
        Ok(Fragment {
            frame_id,
            index,
            count,
            total_len: total_len as u32,
            payload: payload.to_vec(),
        })
    }
}

#[derive(Clone, Debug)]
struct InFlight {
    frame_id: u32,
    count: u16,
    total_len: u32,
    next_index: u16,
    buffer: Vec<u8>,
}

/// 严格有序的帧重组器。
#[derive(Clone, Debug)]
pub struct FragmentAssembler {
    limits: FrameLimits,
    current: Option<InFlight>,
    last_completed: Option<u32>,
}

impl FragmentAssembler {
    /// 创建重组器；预算非法显式拒绝。
    pub fn new(limits: FrameLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            current: None,
            last_completed: None,
        })
    }

    /// 丢弃在途状态（断线重连时使用）。
    pub fn reset(&mut self) {
        self.current = None;
        self.last_completed = None;
    }

    /// 最近一次完整帧编号（诊断/测试用）。
    pub const fn last_completed_frame(&self) -> Option<u32> {
        self.last_completed
    }

    /// 是否存在未收齐的帧。
    pub const fn has_in_flight(&self) -> bool {
        self.current.is_some()
    }

    fn check_fragment(&self, fragment: &Fragment) -> Result<()> {
        if fragment.total_len as usize > self.limits.max_frame_bytes {
            return Err(TransportError::ContentTooLarge {
                size: fragment.total_len as usize,
                limit: self.limits.max_frame_bytes,
            });
        }
        if fragment.count == 0 || fragment.index >= fragment.count {
            return Err(TransportError::MalformedFrame {
                detail: "fragment header out of range",
            });
        }
        if fragment.count as usize > self.limits.max_fragments_per_frame {
            return Err(TransportError::MalformedFrame {
                detail: "fragment count exceeds the configured budget",
            });
        }
        if fragment.payload.len() > self.limits.max_fragment_bytes {
            return Err(TransportError::ContentTooLarge {
                size: fragment.payload.len(),
                limit: self.limits.max_fragment_bytes,
            });
        }
        let chunk = self.limits.max_fragment_bytes;
        if expected_fragment_count(fragment.total_len as usize, chunk) != fragment.count as usize {
            return Err(TransportError::MalformedFrame {
                detail: "fragment count does not match the frame size",
            });
        }
        let expected = canonical_fragment_len(
            fragment.total_len as usize,
            chunk,
            fragment.index as usize,
            fragment.count as usize,
        );
        if fragment.payload.len() != expected {
            return Err(TransportError::MalformedFrame {
                detail: "fragment size does not match canonical chunking",
            });
        }
        Ok(())
    }

    /// 推入一个分片；收齐时返回完整帧字节，否则返回 `None`。
    pub fn push(&mut self, fragment: Fragment) -> Result<Option<Vec<u8>>> {
        self.check_fragment(&fragment)?;
        if let Some(last) = self.last_completed
            && fragment.frame_id <= last
        {
            return Err(TransportError::StaleFrame);
        }
        match &mut self.current {
            None => {
                if fragment.index != 0 {
                    return Err(TransportError::OutOfOrderFragment);
                }
                let mut buffer = Vec::with_capacity(fragment.total_len as usize);
                buffer.extend_from_slice(&fragment.payload);
                if fragment.count == 1 {
                    if buffer.len() != fragment.total_len as usize {
                        return Err(TransportError::IncompleteFrame);
                    }
                    self.last_completed = Some(fragment.frame_id);
                    return Ok(Some(buffer));
                }
                self.current = Some(InFlight {
                    frame_id: fragment.frame_id,
                    count: fragment.count,
                    total_len: fragment.total_len,
                    next_index: 1,
                    buffer,
                });
                Ok(None)
            }
            Some(in_flight) => {
                if fragment.frame_id < in_flight.frame_id {
                    return Err(TransportError::StaleFrame);
                }
                if fragment.frame_id > in_flight.frame_id {
                    // 上一帧尚未收齐就收到下一帧：绝不静默拼接或丢弃。
                    return Err(TransportError::IncompleteFrame);
                }
                if fragment.index < in_flight.next_index {
                    return Err(TransportError::DuplicateFragment);
                }
                if fragment.index > in_flight.next_index {
                    return Err(TransportError::OutOfOrderFragment);
                }
                in_flight.buffer.extend_from_slice(&fragment.payload);
                in_flight.next_index += 1;
                if in_flight.next_index == in_flight.count {
                    if in_flight.buffer.len() != in_flight.total_len as usize {
                        return Err(TransportError::IncompleteFrame);
                    }
                    let frame_id = in_flight.frame_id;
                    let buffer = std::mem::take(&mut in_flight.buffer);
                    self.current = None;
                    self.last_completed = Some(frame_id);
                    Ok(Some(buffer))
                } else {
                    Ok(None)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codec(limits: FrameLimits) -> FrameCodec {
        FrameCodec::new(limits).unwrap()
    }

    #[test]
    fn round_trip_single_and_multi_fragment() {
        let limits = FrameLimits::DEFAULT;
        let codec = codec(limits);
        // 单分片。
        let small = codec.encode(1, b"hello").unwrap();
        assert_eq!(small.len(), 1);
        assert_eq!(small[0].payload(), b"hello");
        let mut assembler = FragmentAssembler::new(limits).unwrap();
        assert_eq!(
            assembler.push(small[0].clone()).unwrap(),
            Some(b"hello".to_vec())
        );
        // 多分片（10 KiB / 4 KiB = 3 片）。
        let frame: Vec<u8> = (0..10 * 1024).map(|index| (index % 251) as u8).collect();
        let fragments = codec.encode(2, &frame).unwrap();
        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0].payload().len(), 4096);
        assert_eq!(fragments[1].payload().len(), 4096);
        assert_eq!(fragments[2].payload().len(), 2048);
        let mut assembler = FragmentAssembler::new(limits).unwrap();
        assert_eq!(assembler.push(fragments[0].clone()).unwrap(), None);
        assert_eq!(assembler.push(fragments[1].clone()).unwrap(), None);
        assert_eq!(assembler.push(fragments[2].clone()).unwrap(), Some(frame));
        assert_eq!(assembler.last_completed_frame(), Some(2));
    }

    #[test]
    fn wire_format_round_trip_and_byte_vectors() {
        let codec = codec(FrameLimits::DEFAULT);
        let fragments = codec.encode(7, b"abc").unwrap();
        let wire = fragments[0].wire_bytes();
        // wire_len = 12 + 3 = 15
        assert_eq!(&wire[..4], &15u32.to_be_bytes());
        assert_eq!(&wire[4..8], &7u32.to_be_bytes());
        assert_eq!(&wire[8..10], &0u16.to_be_bytes());
        assert_eq!(&wire[10..12], &1u16.to_be_bytes());
        assert_eq!(&wire[12..16], &3u32.to_be_bytes());
        assert_eq!(&wire[16..], b"abc");
        let decoded = codec.decode_wire(&wire).unwrap();
        assert_eq!(decoded.frame_id(), 7);
        assert_eq!(decoded.payload(), b"abc");
        // 多分片线格式逐片可解码。
        let frame = vec![9u8; 5000];
        for wire in codec.encode_wire(8, &frame).unwrap() {
            let fragment = codec.decode_wire(&wire).unwrap();
            assert_eq!(fragment.frame_id(), 8);
            assert_eq!(fragment.total_len(), 5000);
        }
    }

    #[test]
    fn frame_and_fragment_boundaries_are_enforced() {
        let limits = FrameLimits {
            max_frame_bytes: 16,
            max_fragment_bytes: 4,
            max_fragments_per_frame: 4,
        };
        let codec = codec(limits);
        // 恰好 16 字节。
        assert_eq!(codec.encode(1, &[1u8; 16]).unwrap().len(), 4);
        // 17 字节超限。
        assert_eq!(
            codec.encode(1, &[1u8; 17]).unwrap_err().code(),
            "CONTENT_TOO_LARGE"
        );
        // 空帧拒绝。
        assert_eq!(codec.encode(1, b"").unwrap_err().code(), "MALFORMED_FRAME");
        // 非法预算。
        for limits in [
            FrameLimits {
                max_frame_bytes: 0,
                max_fragment_bytes: 1,
                max_fragments_per_frame: 1,
            },
            FrameLimits {
                max_frame_bytes: 16,
                max_fragment_bytes: 0,
                max_fragments_per_frame: 4,
            },
            FrameLimits {
                max_frame_bytes: 16,
                max_fragment_bytes: 32,
                max_fragments_per_frame: 4,
            },
            FrameLimits {
                max_frame_bytes: 16,
                max_fragment_bytes: 4,
                max_fragments_per_frame: 0,
            },
            FrameLimits {
                max_frame_bytes: 16,
                max_fragment_bytes: 4,
                max_fragments_per_frame: 2,
            },
        ] {
            assert_eq!(
                FrameCodec::new(limits).unwrap_err().code(),
                "INVALID_FRAME_LIMITS"
            );
        }
    }

    #[test]
    fn malformed_wire_is_rejected_before_allocation() {
        let codec = codec(FrameLimits {
            max_frame_bytes: 16,
            max_fragment_bytes: 8,
            max_fragments_per_frame: 2,
        });
        let good = codec.encode(1, b"abcdefgh").unwrap()[0].wire_bytes();
        // 截断长度前缀。
        assert!(codec.decode_wire(&[]).is_err());
        assert!(codec.decode_wire(&good[..3]).is_err());
        // 长度前缀不匹配。
        assert!(codec.decode_wire(&good[..good.len() - 1]).is_err());
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(codec.decode_wire(&trailing).is_err());
        // total_len 为 0。
        let mut zero_total = good.clone();
        zero_total[12..16].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(
            codec.decode_wire(&zero_total).unwrap_err().code(),
            "MALFORMED_FRAME"
        );
        // 超长声明（不分配内存就拒绝）。
        let mut huge = good.clone();
        huge[12..16].copy_from_slice(&1_000_000u32.to_be_bytes());
        assert_eq!(
            codec.decode_wire(&huge).unwrap_err().code(),
            "CONTENT_TOO_LARGE"
        );
        // 分片数 0 / 索引越界 / 分片数超预算。
        let mut zero_count = good.clone();
        zero_count[10..12].copy_from_slice(&0u16.to_be_bytes());
        assert!(codec.decode_wire(&zero_count).is_err());
        let mut bad_index = good.clone();
        bad_index[8..10].copy_from_slice(&2u16.to_be_bytes());
        assert!(codec.decode_wire(&bad_index).is_err());
        let mut too_many = good.clone();
        too_many[10..12].copy_from_slice(&3u16.to_be_bytes());
        assert!(codec.decode_wire(&too_many).is_err());
        // 非规范切分：16 字节帧声称 1 片但只带 8 字节。
        let mut short_payload = good.clone();
        short_payload.truncate(FRAGMENT_HEADER_BYTES + 4);
        let short_wire_len = (short_payload.len() - 4) as u32;
        short_payload[0..4].copy_from_slice(&short_wire_len.to_be_bytes());
        short_payload[12..16].copy_from_slice(&16u32.to_be_bytes());
        assert_eq!(
            codec.decode_wire(&short_payload).unwrap_err().code(),
            "MALFORMED_FRAME"
        );
    }

    #[test]
    fn assembler_rejects_duplicate_out_of_order_stale_and_incomplete() {
        let limits = FrameLimits {
            max_frame_bytes: 16,
            max_fragment_bytes: 4,
            max_fragments_per_frame: 4,
        };
        let codec = codec(limits);
        let fragments = codec.encode(1, &[1u8; 12]).unwrap();
        assert_eq!(fragments.len(), 3);
        let mut assembler = FragmentAssembler::new(limits).unwrap();
        // 乱序：跳过索引 0。
        assert_eq!(
            assembler.push(fragments[1].clone()).unwrap_err().code(),
            "OUT_OF_ORDER_FRAGMENT"
        );
        assert_eq!(assembler.push(fragments[0].clone()).unwrap(), None);
        // 重复：再次索引 0。
        assert_eq!(
            assembler.push(fragments[0].clone()).unwrap_err().code(),
            "DUPLICATE_FRAGMENT"
        );
        // 乱序：跳过索引 1 直接索引 2。
        assert_eq!(
            assembler.push(fragments[2].clone()).unwrap_err().code(),
            "OUT_OF_ORDER_FRAGMENT"
        );
        // 未收齐就换帧。
        let next_frame = codec.encode(2, &[2u8; 8]).unwrap();
        assert_eq!(
            assembler.push(next_frame[0].clone()).unwrap_err().code(),
            "INCOMPLETE_FRAME"
        );
        // 补齐本帧。
        assert_eq!(assembler.push(fragments[1].clone()).unwrap(), None);
        assert_eq!(
            assembler.push(fragments[2].clone()).unwrap(),
            Some(vec![1u8; 12])
        );
        assert_eq!(assembler.last_completed_frame(), Some(1));
        // 旧帧迟到。
        assert_eq!(
            assembler.push(fragments[0].clone()).unwrap_err().code(),
            "STALE_FRAME"
        );
        // 新帧按序完成（8 字节 / 4 字节分片 = 2 片）。
        assert_eq!(next_frame.len(), 2);
        assert_eq!(assembler.push(next_frame[0].clone()).unwrap(), None);
        assert_eq!(
            assembler.push(next_frame[1].clone()).unwrap(),
            Some(vec![2u8; 8])
        );
        // reset 后可重新接收。
        assembler.reset();
        assert!(!assembler.has_in_flight());
        assert_eq!(assembler.last_completed_frame(), None);
        assert_eq!(assembler.push(fragments[0].clone()).unwrap(), None);
    }

    #[test]
    fn random_wire_input_never_panics() {
        let codec = codec(FrameLimits::DEFAULT);
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for length in 0..256usize {
            let mut wire = vec![0u8; length];
            for byte in &mut wire {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                *byte = (state >> 33) as u8;
            }
            let _ = codec.decode_wire(&wire);
        }
        // 合理记录但随机字段也不 panic。
        let mut wire = codec.encode(1, b"payload").unwrap()[0].wire_bytes();
        for trial in 0..64u64 {
            let position = 4 + (trial % 12) as usize;
            wire[position] ^= (trial as u8).wrapping_add(1);
            let _ = codec.decode_wire(&wire);
            wire[position] ^= (trial as u8).wrapping_add(1);
        }
    }
}
