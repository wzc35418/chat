//! TRANSPORT-007 的纯逻辑子集：把 sync 的 `Pull { from, to }` 计划翻译成帧请求序列。
//!
//! 不依赖 `chat-sync`（保持 transport 在 sync 之下），本地 `PullRange` 与 sync 的
//! `PullAction::Pull { from, to }` 字段一一对应，由 core 层转换。
//!
//! 语义：
//!
//! - **分页**：按 `page_items`（1..=500）切分闭区间 `[from, to]`，每页条数 ≤ page_items；
//! - **序号绑定**：每个请求有 0 起 `request_index`，帧号 = `base_frame_id + index`
//!   （溢出即拒绝），因此重放/乱序能被识别；
//! - **去重**：整个计划有确定性 SHA-256 摘要（范围/分页/帧号全部绑定），相同输入
//!   得到相同摘要，可直接用于幂等去重；
//! - **响应校验**：`ResponseValidator` 要求按序、范围与条目数匹配；重复响应
//!   `DUPLICATE_RESPONSE`，不一致 `RESPONSE_MISMATCH`。
//!
//! 请求正文编码属于节点 API（node-api）职责，本模块只产出请求描述；不含 HTTP。

use std::collections::BTreeSet;

use sha2::{Digest as _, Sha256};

use crate::error::{Result, TransportError};

/// 默认分页条数。
pub const DEFAULT_PULL_PAGE_ITEMS: u64 = 100;
/// 单页条数硬上限。
pub const MAX_PULL_PAGE_ITEMS: u64 = 500;
/// 单个计划请求数硬上限。
pub const MAX_REQUESTS_PER_PLAN: usize = 256;
/// 计划摘要用途域。
pub const PULL_PLAN_DOMAIN: &[u8] = b"chat/p1/transport-pull-plan/v1";

/// sync 拉取区间（闭区间，序号从 1 开始）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PullRange {
    /// 起始序号。
    pub from: u64,
    /// 结束序号（含）。
    pub to: u64,
}

impl PullRange {
    /// 校验并构造区间。
    pub fn new(from: u64, to: u64) -> Result<Self> {
        let range = Self { from, to };
        range.validate()?;
        Ok(range)
    }

    /// 区间条数。
    pub fn len(&self) -> u64 {
        self.to - self.from + 1
    }

    /// 是否为空区间（合法区间至少 1 条）。
    pub fn is_empty(&self) -> bool {
        self.from > self.to
    }

    fn validate(&self) -> Result<()> {
        if self.from == 0 {
            return Err(TransportError::PullPlanInvalid {
                detail: "pull range must start at 1",
            });
        }
        if self.from > self.to {
            return Err(TransportError::PullPlanInvalid {
                detail: "pull range end precedes its start",
            });
        }
        if self.to > chat_protocol::MAX_CURSOR_SEQ {
            return Err(TransportError::PullPlanInvalid {
                detail: "pull range exceeds the storable sequence range",
            });
        }
        Ok(())
    }
}

/// 一页请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    request_index: u32,
    from: u64,
    to: u64,
    frame_id: u32,
}

impl PageRequest {
    /// 0 起请求序号。
    pub const fn request_index(&self) -> u32 {
        self.request_index
    }

    /// 页起始序号。
    pub const fn from(&self) -> u64 {
        self.from
    }

    /// 页结束序号（含）。
    pub const fn to(&self) -> u64 {
        self.to
    }

    /// 绑定帧号。
    pub const fn frame_id(&self) -> u32 {
        self.frame_id
    }

    /// 页条数。
    pub const fn items(&self) -> u64 {
        self.to - self.from + 1
    }
}

/// 完整拉取计划。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestPlan {
    range: PullRange,
    page_items: u64,
    total_items: u64,
    requests: Vec<PageRequest>,
    digest: [u8; 32],
}

impl PullRequestPlan {
    /// 原始区间。
    pub const fn range(&self) -> PullRange {
        self.range
    }

    /// 分页条数。
    pub const fn page_items(&self) -> u64 {
        self.page_items
    }

    /// 区间总条数。
    pub const fn total_items(&self) -> u64 {
        self.total_items
    }

    /// 请求序列（按 request_index 升序）。
    pub fn requests(&self) -> &[PageRequest] {
        &self.requests
    }

    /// 确定性计划摘要（32 字节）。
    pub const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// 计划摘要小写十六进制。
    pub fn digest_hex(&self) -> String {
        let mut text = String::with_capacity(64);
        for byte in self.digest {
            text.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
            text.push(char::from(b"0123456789abcdef"[(byte & 0x0f) as usize]));
        }
        text
    }
}

fn compute_digest(range: PullRange, page_items: u64, requests: &[PageRequest]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PULL_PLAN_DOMAIN);
    hasher.update(range.from.to_be_bytes());
    hasher.update(range.to.to_be_bytes());
    hasher.update(page_items.to_be_bytes());
    hasher.update((requests.len() as u32).to_be_bytes());
    for request in requests {
        hasher.update(request.request_index.to_be_bytes());
        hasher.update(request.from.to_be_bytes());
        hasher.update(request.to.to_be_bytes());
        hasher.update(request.frame_id.to_be_bytes());
    }
    hasher.finalize().into()
}

/// 把区间翻译成帧请求序列。
///
/// 失败（`PULL_PLAN_INVALID`）：非法区间、分页为 0 或超过 500、请求数超过
/// `max_requests`（且 >256 恒拒绝）、帧号溢出。
pub fn plan_pull_requests(
    range: PullRange,
    page_items: u64,
    base_frame_id: u32,
    max_requests: usize,
) -> Result<PullRequestPlan> {
    range.validate()?;
    if page_items == 0 || page_items > MAX_PULL_PAGE_ITEMS {
        return Err(TransportError::PullPlanInvalid {
            detail: "pageItems must be within 1..=500",
        });
    }
    if max_requests == 0 || max_requests > MAX_REQUESTS_PER_PLAN {
        return Err(TransportError::PullPlanInvalid {
            detail: "maxRequests must be within 1..=256",
        });
    }
    let total_items = range.len();
    let count = total_items.div_ceil(page_items);
    if count > max_requests as u64 {
        return Err(TransportError::PullPlanInvalid {
            detail: "pull plan exceeds maxRequests",
        });
    }
    let mut requests = Vec::with_capacity(count as usize);
    for index in 0..count {
        let offset = index
            .checked_mul(page_items)
            .ok_or(TransportError::PullPlanInvalid {
                detail: "pull plan offset overflow",
            })?;
        let from = range
            .from
            .checked_add(offset)
            .ok_or(TransportError::PullPlanInvalid {
                detail: "pull plan range overflow",
            })?;
        let to = from.saturating_add(page_items - 1).min(range.to);
        let request_index = u32::try_from(index).map_err(|_| TransportError::PullPlanInvalid {
            detail: "pull plan request index overflow",
        })?;
        let frame_id =
            base_frame_id
                .checked_add(request_index)
                .ok_or(TransportError::PullPlanInvalid {
                    detail: "pull plan frame id overflow",
                })?;
        requests.push(PageRequest {
            request_index,
            from,
            to,
            frame_id,
        });
    }
    let digest = compute_digest(range, page_items, &requests);
    Ok(PullRequestPlan {
        range,
        page_items,
        total_items,
        requests,
        digest,
    })
}

/// 一页响应（由调用方从响应体解析后传入；本层不解析 HTTP）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageResponse {
    /// 对应请求序号。
    pub request_index: u32,
    /// 响应起始序号。
    pub from: u64,
    /// 响应结束序号（含）。
    pub to: u64,
    /// 响应条目数。
    pub items: u32,
}

/// 通过校验的响应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedPage {
    /// 请求序号。
    pub request_index: u32,
    /// 绑定帧号。
    pub frame_id: u32,
    /// 起始序号。
    pub from: u64,
    /// 结束序号（含）。
    pub to: u64,
    /// 条目数。
    pub items: u32,
}

/// 响应校验器：要求按请求序号顺序响应、范围与条目数匹配、拒绝重复。
#[derive(Clone, Debug)]
pub struct ResponseValidator {
    expected: Vec<PageRequest>,
    next_index: u32,
    accepted: BTreeSet<u32>,
}

impl ResponseValidator {
    /// 由计划创建校验器；空计划拒绝（合法计划至少一页）。
    pub fn new(plan: &PullRequestPlan) -> Result<Self> {
        if plan.requests.is_empty() {
            return Err(TransportError::PullPlanInvalid {
                detail: "pull plan has no requests",
            });
        }
        Ok(Self {
            expected: plan.requests.clone(),
            next_index: 0,
            accepted: BTreeSet::new(),
        })
    }

    /// 已接受响应数。
    pub fn accepted_count(&self) -> usize {
        self.accepted.len()
    }

    /// 是否全部响应已接受。
    pub fn is_complete(&self) -> bool {
        self.accepted.len() == self.expected.len()
    }

    /// 尚未响应的请求序号（升序）。
    pub fn missing_requests(&self) -> Vec<u32> {
        (0..self.expected.len() as u32)
            .filter(|index| !self.accepted.contains(index))
            .collect()
    }

    /// 校验并接受一页响应。
    ///
    /// 顺序：索引范围 → 重复 → 顺序 → 范围 → 条目数。失败不改变校验器状态。
    pub fn accept(&mut self, response: PageResponse) -> Result<AcceptedPage> {
        let Some(expected) = self.expected.get(response.request_index as usize) else {
            return Err(TransportError::ResponseMismatch {
                detail: "response request index is out of range",
            });
        };
        if self.accepted.contains(&response.request_index) {
            return Err(TransportError::DuplicateResponse);
        }
        if response.request_index != self.next_index {
            return Err(TransportError::ResponseMismatch {
                detail: "response is out of order",
            });
        }
        if response.from != expected.from || response.to != expected.to {
            return Err(TransportError::ResponseMismatch {
                detail: "responded range does not match the request",
            });
        }
        let expected_items = response
            .to
            .checked_sub(response.from)
            .and_then(|span| span.checked_add(1))
            .ok_or(TransportError::ResponseMismatch {
                detail: "responded range is inverted",
            })?;
        if u64::from(response.items) != expected_items {
            return Err(TransportError::ResponseMismatch {
                detail: "responded item count does not match the range",
            });
        }
        self.accepted.insert(response.request_index);
        self.next_index = response.request_index + 1;
        Ok(AcceptedPage {
            request_index: response.request_index,
            frame_id: expected.frame_id,
            from: response.from,
            to: response.to,
            items: response.items,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST_VECTOR: &str = "06003b27ee4a07f024ec0f987ec914f90336db2029cc7d8e2a710313a1a45ba2";

    fn range(from: u64, to: u64) -> PullRange {
        PullRange::new(from, to).unwrap()
    }

    fn make_plan(from: u64, to: u64, page_items: u64) -> PullRequestPlan {
        plan_pull_requests(range(from, to), page_items, 100, MAX_REQUESTS_PER_PLAN).unwrap()
    }

    #[test]
    fn paginates_inclusive_ranges() {
        let plan = make_plan(1, 250, 100);
        assert_eq!(plan.total_items(), 250);
        assert_eq!(plan.requests().len(), 3);
        assert_eq!(
            plan.requests()
                .iter()
                .map(|request| (
                    request.request_index(),
                    request.from(),
                    request.to(),
                    request.frame_id()
                ))
                .collect::<Vec<_>>(),
            vec![(0, 1, 100, 100), (1, 101, 200, 101), (2, 201, 250, 102)]
        );
        assert_eq!(plan.requests()[2].items(), 50);
        // 单条与整除边界。
        let single = make_plan(5, 5, 100);
        assert_eq!(single.requests().len(), 1);
        assert_eq!(
            (single.requests()[0].from(), single.requests()[0].to()),
            (5, 5)
        );
        let exact = make_plan(1, 300, 100);
        assert_eq!(exact.requests().len(), 3);
        assert_eq!(exact.requests()[2].to(), 300);
        // 每页一条。
        let tiny = make_plan(7, 10, 1);
        assert_eq!(tiny.requests().len(), 4);
        assert_eq!(tiny.requests()[3].to(), 10);
    }

    #[test]
    fn rejects_invalid_ranges_pages_and_request_counts() {
        for (from, to, page, max_requests) in [
            (0, 5, 100, 10),
            (6, 5, 100, 10),
            (1, 5, 0, 10),
            (1, 5, MAX_PULL_PAGE_ITEMS + 1, 10),
            (1, 5, 100, 0),
            (1, 5, 100, MAX_REQUESTS_PER_PLAN + 1),
            (1, 1_000, 1, 10),
        ] {
            assert_eq!(
                plan_pull_requests(PullRange { from, to }, page, 1, max_requests)
                    .unwrap_err()
                    .code(),
                "PULL_PLAN_INVALID",
                "from={from} to={to} page={page} max={max_requests}"
            );
        }
        // 帧号溢出。
        assert_eq!(
            plan_pull_requests(range(1, 200), 100, u32::MAX, 4)
                .unwrap_err()
                .code(),
            "PULL_PLAN_INVALID"
        );
        // 超出可存储序号范围。
        assert_eq!(
            PullRange::new(1, chat_protocol::MAX_CURSOR_SEQ + 1)
                .unwrap_err()
                .code(),
            "PULL_PLAN_INVALID"
        );
        assert!(PullRange::new(1, 1).is_ok());
        assert!(!PullRange::new(1, 2).unwrap().is_empty());
    }

    #[test]
    fn plan_digest_is_deterministic_and_bound() {
        let first = make_plan(1, 250, 100);
        let same = make_plan(1, 250, 100);
        assert_eq!(first.digest(), same.digest());
        assert_eq!(first.digest_hex(), DIGEST_VECTOR);
        // 页大小、范围、帧号基址都会改变摘要。
        assert_ne!(first.digest(), make_plan(1, 250, 50).digest());
        assert_ne!(first.digest(), make_plan(1, 251, 100).digest());
        assert_ne!(
            first.digest(),
            plan_pull_requests(range(1, 250), 100, 200, MAX_REQUESTS_PER_PLAN)
                .unwrap()
                .digest()
        );
        assert_eq!(first.digest_hex().len(), 64);
    }

    #[test]
    fn responses_are_validated_in_order_without_duplicates() {
        let plan = make_plan(1, 250, 100);
        let mut validator = ResponseValidator::new(&plan).unwrap();
        assert_eq!(validator.missing_requests(), vec![0, 1, 2]);
        let first = validator
            .accept(PageResponse {
                request_index: 0,
                from: 1,
                to: 100,
                items: 100,
            })
            .unwrap();
        assert_eq!(
            first,
            AcceptedPage {
                request_index: 0,
                frame_id: 100,
                from: 1,
                to: 100,
                items: 100
            }
        );
        assert_eq!(validator.missing_requests(), vec![1, 2]);
        // 重复响应。
        assert_eq!(
            validator
                .accept(PageResponse {
                    request_index: 0,
                    from: 1,
                    to: 100,
                    items: 100
                })
                .unwrap_err()
                .code(),
            "DUPLICATE_RESPONSE"
        );
        // 乱序（跳过 1 直接给 2）。
        assert_eq!(
            validator
                .accept(PageResponse {
                    request_index: 2,
                    from: 201,
                    to: 250,
                    items: 50
                })
                .unwrap_err()
                .code(),
            "RESPONSE_MISMATCH"
        );
        // 范围不符。
        assert_eq!(
            validator
                .accept(PageResponse {
                    request_index: 1,
                    from: 101,
                    to: 199,
                    items: 99
                })
                .unwrap_err()
                .code(),
            "RESPONSE_MISMATCH"
        );
        // 条目数不符。
        assert_eq!(
            validator
                .accept(PageResponse {
                    request_index: 1,
                    from: 101,
                    to: 200,
                    items: 99
                })
                .unwrap_err()
                .code(),
            "RESPONSE_MISMATCH"
        );
        // 索引越界。
        assert_eq!(
            validator
                .accept(PageResponse {
                    request_index: 9,
                    from: 1,
                    to: 1,
                    items: 1
                })
                .unwrap_err()
                .code(),
            "RESPONSE_MISMATCH"
        );
        // 正确补齐。
        assert!(
            validator
                .accept(PageResponse {
                    request_index: 1,
                    from: 101,
                    to: 200,
                    items: 100
                })
                .is_ok()
        );
        assert!(!validator.is_complete());
        assert!(
            validator
                .accept(PageResponse {
                    request_index: 2,
                    from: 201,
                    to: 250,
                    items: 50
                })
                .is_ok()
        );
        assert!(validator.is_complete());
        assert_eq!(validator.accepted_count(), 3);
        assert!(validator.missing_requests().is_empty());
    }
}
