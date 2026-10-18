//! SYNC-003 / SYNC-004 发送意图调度测试。
//!
//! 覆盖：同草稿同 SubmissionId（二次调度幂等、只加密一次）、unknown 只对账不重发
//! 不重新加密、租约门（有效租约禁止并发重发 / 过期才允许同字节重发）、成功才释放、
//! 拒绝后重建得到新 ID、草稿取消、投影不一致与预算越界的有界拒绝。

use chat_protocol::SubmissionId;
use chat_sync::send::{
    AttemptSnapshot, AttemptState, DraftSnapshot, DraftState, MAX_ATTEMPT_INDEX, SubmissionPlan,
    derive_attempt_submission_id, plan_submission, reject_unknown_resend,
};
use chat_sync::ErrorCode;

const NOW: i64 = 1_700_000_000_000;
const DRAFT: &str = "draft-1";
const WORKER: &str = "worker-a";

fn make_draft(state: DraftState, epoch: u64, attempt_index: u32) -> DraftSnapshot {
    DraftSnapshot {
        draft_id: DRAFT.to_owned(),
        state,
        mls_epoch: epoch,
        attempt_index,
    }
}

fn derive_id(draft_id: &str, epoch: u64, attempt_index: u32) -> SubmissionId {
    derive_attempt_submission_id(draft_id, epoch, attempt_index).expect("derive")
}

fn make_attempt(
    state: AttemptState,
    epoch: u64,
    lease_owner: Option<&str>,
    lease_until_ms: Option<i64>,
) -> AttemptSnapshot {
    AttemptSnapshot {
        submission_id: derive_id(DRAFT, epoch, 0),
        draft_id: DRAFT.to_owned(),
        state,
        mls_epoch: epoch,
        lease_owner: lease_owner.map(str::to_owned),
        lease_until_ms,
        lease_version: 1,
    }
}

#[test]
fn first_scheduling_derives_one_submission_id_and_is_idempotent() {
    let draft = make_draft(DraftState::Queued, 7, 0);
    let first = plan_submission(&draft, None, WORKER, NOW).expect("plan");
    let second = plan_submission(&draft, None, WORKER, NOW + 5_000).expect("plan");
    assert_eq!(first, second, "same inputs must produce the same plan and ID");
    match &first {
        SubmissionPlan::Prepare {
            draft_id,
            submission_id,
            mls_epoch,
            attempt_index,
        } => {
            assert_eq!(draft_id, DRAFT);
            assert_eq!(*mls_epoch, 7);
            assert_eq!(*attempt_index, 0);
            let text = submission_id.encode();
            assert!(text.starts_with("sub:1:"), "PROTOCOL-001 text form: {text}");
            assert_eq!(
                SubmissionId::parse(&text).expect("round trip"),
                *submission_id,
                "SubmissionId must be a canonical protocol ID"
            );
        }
        other => panic!("expected Prepare, got {other:?}"),
    }
    assert!(first.requires_encryption());
    assert!(!first.allows_resend());
    assert_eq!(first.submission_id(), Some(derive_id(DRAFT, 7, 0)));
}

#[test]
fn derived_ids_are_stable_and_domain_separated() {
    let base = derive_id(DRAFT, 7, 0);
    assert_eq!(base, derive_id(DRAFT, 7, 0));
    assert_ne!(base, derive_id(DRAFT, 8, 0), "epoch changes the attempt");
    assert_ne!(base, derive_id(DRAFT, 7, 1), "rebuild changes the attempt");
    assert_ne!(
        base,
        derive_id("draft-2", 7, 0),
        "drafts never share a submission ID"
    );
    assert_eq!(
        derive_attempt_submission_id(DRAFT, 7, MAX_ATTEMPT_INDEX + 1)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
    assert_eq!(
        derive_attempt_submission_id("bad draft!", 7, 0).unwrap_err().code(),
        ErrorCode::InvalidId
    );
}

#[test]
fn prepared_attempt_is_resubmitted_with_the_same_id_and_bytes() {
    let draft = make_draft(DraftState::Queued, 7, 0);
    let snapshot = make_attempt(AttemptState::Prepared, 7, None, None);
    let plan = plan_submission(&draft, Some(&snapshot), WORKER, NOW).expect("plan");
    match &plan {
        SubmissionPlan::Resubmit {
            submission_id,
            was_submitted,
            lease_version,
        } => {
            assert_eq!(*submission_id, snapshot.submission_id);
            assert!(!was_submitted);
            assert_eq!(*lease_version, 1);
        }
        other => panic!("expected Resubmit, got {other:?}"),
    }
    assert!(plan.allows_resend());
    assert!(!plan.requires_encryption(), "resend must not re-encrypt");
}

#[test]
fn active_lease_blocks_concurrent_resend_until_it_expires() {
    let draft = make_draft(DraftState::Queued, 7, 0);
    let snapshot = make_attempt(
        AttemptState::Submitted,
        7,
        Some("worker-b"),
        Some(NOW + 30_000),
    );
    let mine = plan_submission(&draft, Some(&snapshot), "worker-b", NOW).expect("plan");
    match &mine {
        SubmissionPlan::WaitForLease {
            lease_until_ms,
            held_by_current_worker,
            ..
        } => {
            assert_eq!(*lease_until_ms, NOW + 30_000);
            assert!(held_by_current_worker);
        }
        other => panic!("expected WaitForLease, got {other:?}"),
    }
    assert!(!mine.allows_resend());
    let other = plan_submission(&draft, Some(&snapshot), WORKER, NOW).expect("plan");
    match &other {
        SubmissionPlan::WaitForLease {
            held_by_current_worker,
            ..
        } => assert!(!held_by_current_worker),
        other => panic!("expected WaitForLease, got {other:?}"),
    }

    // 到期后允许同 ID 同字节重发。
    let expired = plan_submission(&draft, Some(&snapshot), WORKER, NOW + 30_000).expect("plan");
    match &expired {
        SubmissionPlan::Resubmit {
            submission_id,
            was_submitted,
            ..
        } => {
            assert_eq!(*submission_id, snapshot.submission_id);
            assert!(was_submitted);
        }
        other => panic!("expected Resubmit, got {other:?}"),
    }
}

#[test]
fn unknown_is_never_resent_even_after_lease_expiry() {
    let draft = make_draft(DraftState::Queued, 7, 0);
    let snapshot = make_attempt(
        AttemptState::Unknown,
        7,
        Some("worker-old"),
        Some(NOW - 60_000),
    );
    for now in [NOW, NOW + 3_600_000] {
        let plan = plan_submission(&draft, Some(&snapshot), WORKER, now).expect("plan");
        assert_eq!(
            plan,
            SubmissionPlan::QueryExisting {
                submission_id: snapshot.submission_id
            }
        );
        assert!(!plan.allows_resend(), "unknown must never be resent");
        assert!(
            !plan.requires_encryption(),
            "unknown must not trigger a second encryption"
        );
        assert!(!plan.is_terminal(), "unknown stays pending reconciliation");
    }
    let error = reject_unknown_resend(&snapshot).expect_err("guard must reject resend");
    assert_eq!(error.code(), ErrorCode::UnknownResult);
    // 非 unknown 尝试不受该守卫影响。
    let prepared = make_attempt(AttemptState::Prepared, 7, None, None);
    reject_unknown_resend(&prepared).expect("prepared can be resent");
}

#[test]
fn only_formal_acceptance_releases_the_attempt() {
    let draft = make_draft(DraftState::Queued, 7, 0);
    let snapshot = make_attempt(AttemptState::Accepted, 7, None, None);
    let plan = plan_submission(&draft, Some(&snapshot), WORKER, NOW).expect("plan");
    assert_eq!(
        plan,
        SubmissionPlan::Release {
            submission_id: snapshot.submission_id
        }
    );
    assert!(plan.is_terminal());
    assert!(!plan.allows_resend());
    // 重复调度保持 Release（幂等，不再提交）。
    assert_eq!(
        plan_submission(&draft, Some(&snapshot), WORKER, NOW + 1).unwrap(),
        plan
    );
}

#[test]
fn rejection_rebuilds_with_a_new_submission_id() {
    let draft = make_draft(DraftState::Queued, 7, 0);
    let snapshot = make_attempt(AttemptState::Rejected, 7, None, None);
    let plan = plan_submission(&draft, Some(&snapshot), WORKER, NOW).expect("plan");
    match &plan {
        SubmissionPlan::Rebuild {
            rejected_submission_id,
            submission_id,
            attempt_index,
            mls_epoch,
            ..
        } => {
            assert_eq!(*rejected_submission_id, snapshot.submission_id);
            assert_ne!(*submission_id, snapshot.submission_id);
            assert_eq!(*attempt_index, 1);
            assert_eq!(*mls_epoch, 7);
            assert_eq!(*submission_id, derive_id(DRAFT, 7, 1));
        }
        other => panic!("expected Rebuild, got {other:?}"),
    }
    assert!(!plan.allows_resend(), "rebuild needs a new encryption, not a resend");
    assert!(!plan.is_terminal());
    // 重建预算耗尽：显式拒绝。
    let exhausted = make_draft(DraftState::Queued, 7, MAX_ATTEMPT_INDEX);
    assert_eq!(
        plan_submission(&exhausted, Some(&snapshot), WORKER, NOW)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
}

#[test]
fn cancelled_draft_abandons_every_path() {
    let cancelled = make_draft(DraftState::Cancelled, 7, 0);
    assert_eq!(
        plan_submission(&cancelled, None, WORKER, NOW).unwrap(),
        SubmissionPlan::Abandoned {
            draft_id: DRAFT.to_owned()
        }
    );
    for state in [AttemptState::Prepared, AttemptState::Rejected] {
        let snapshot = make_attempt(state, 7, None, None);
        let plan = plan_submission(&cancelled, Some(&snapshot), WORKER, NOW).expect("plan");
        assert!(matches!(plan, SubmissionPlan::Abandoned { .. }));
        assert!(plan.is_terminal());
    }
    // 已正式接受的尝试仍按 Release 处理（不因草稿取消而丢掉成功事实）。
    let accepted = make_attempt(AttemptState::Accepted, 7, None, None);
    assert!(matches!(
        plan_submission(&cancelled, Some(&accepted), WORKER, NOW).unwrap(),
        SubmissionPlan::Release { .. }
    ));
}

#[test]
fn inconsistent_or_malformed_snapshots_are_rejected() {
    let queued = make_draft(DraftState::Queued, 7, 0);
    // 尝试属于别的草稿。
    let mut foreign = make_attempt(AttemptState::Prepared, 7, None, None);
    foreign.draft_id = "draft-other".to_owned();
    assert_eq!(
        plan_submission(&queued, Some(&foreign), WORKER, NOW)
            .unwrap_err()
            .code(),
        ErrorCode::StateCorrupted
    );
    // 租约只有截止时间没有持有者。
    let mut dangling = make_attempt(AttemptState::Prepared, 7, None, None);
    dangling.lease_until_ms = Some(NOW + 1_000);
    assert_eq!(
        plan_submission(&queued, Some(&dangling), WORKER, NOW)
            .unwrap_err()
            .code(),
        ErrorCode::StateCorrupted
    );
    // 负租约时间。
    let mut negative = make_attempt(AttemptState::Prepared, 7, Some(WORKER), Some(-1));
    negative.lease_until_ms = Some(-1);
    assert_eq!(
        plan_submission(&queued, Some(&negative), WORKER, NOW)
            .unwrap_err()
            .code(),
        ErrorCode::StateCorrupted
    );
    // 草稿已接受但尝试没有接受。
    let accepted_draft = make_draft(DraftState::Accepted, 7, 0);
    let prepared = make_attempt(AttemptState::Prepared, 7, None, None);
    assert_eq!(
        plan_submission(&accepted_draft, Some(&prepared), WORKER, NOW)
            .unwrap_err()
            .code(),
        ErrorCode::StateCorrupted
    );
    assert_eq!(
        plan_submission(&accepted_draft, None, WORKER, NOW)
            .unwrap_err()
            .code(),
        ErrorCode::StateCorrupted
    );
    // 参数与上限。
    assert_eq!(
        plan_submission(&queued, None, "bad worker!", NOW)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidId
    );
    assert_eq!(
        plan_submission(&queued, None, WORKER, -1).unwrap_err().code(),
        ErrorCode::InvalidTarget
    );
    let over_budget = make_draft(DraftState::Queued, 7, MAX_ATTEMPT_INDEX + 1);
    assert_eq!(
        plan_submission(&over_budget, None, WORKER, NOW)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
    assert_eq!(
        plan_submission(
            &DraftSnapshot {
                draft_id: "bad id!".to_owned(),
                state: DraftState::Queued,
                mls_epoch: 7,
                attempt_index: 0,
            },
            None,
            WORKER,
            NOW
        )
        .unwrap_err()
        .code(),
        ErrorCode::InvalidId
    );
}

#[test]
fn the_plan_sequence_encrypts_exactly_once() {
    // 用一个假执行器按计划推进：加密/提交计数必须只发生一次，unknown 后只查询。
    let draft = make_draft(DraftState::Queued, 7, 0);
    let mut encrypt_calls = 0u32;
    let mut submit_calls = 0u32;
    let mut query_calls = 0u32;
    let mut plan = plan_submission(&draft, None, WORKER, NOW).expect("plan");
    let mut current: Option<AttemptSnapshot> = None;
    for step in 0..6 {
        match &plan {
            SubmissionPlan::Prepare { submission_id, .. } => {
                encrypt_calls += 1;
                // 假持久化：把 Plan 变成耐久尝试投影。
                current = Some(AttemptSnapshot {
                    submission_id: *submission_id,
                    draft_id: DRAFT.to_owned(),
                    state: AttemptState::Prepared,
                    mls_epoch: 7,
                    lease_owner: None,
                    lease_until_ms: None,
                    lease_version: 0,
                });
            }
            SubmissionPlan::Resubmit { .. } => submit_calls += 1,
            SubmissionPlan::QueryExisting { .. } => query_calls += 1,
            SubmissionPlan::WaitForLease { .. } => {}
            SubmissionPlan::Release { .. } | SubmissionPlan::Abandoned { .. } => {}
            SubmissionPlan::Rebuild { .. } => panic!("no rebuild expected in this flow"),
        }
        assert_eq!(
            *plan.submission_id().as_ref().unwrap(),
            derive_id(DRAFT, 7, 0),
            "every step must keep the same SubmissionId"
        );
        if step == 2
            && let Some(snapshot) = current.as_mut()
        {
            // 结果未知：核心把尝试标成 unknown。
            snapshot.state = AttemptState::Unknown;
            snapshot.lease_owner = Some(WORKER.to_owned());
            snapshot.lease_until_ms = Some(NOW - 1);
        }
        if step == 4
            && let Some(snapshot) = current.as_mut()
        {
            snapshot.state = AttemptState::Accepted;
        }
        plan = plan_submission(&draft, current.as_ref(), WORKER, NOW + step as i64).expect("plan");
    }
    assert_eq!(encrypt_calls, 1, "exactly one encryption for one draft");
    assert_eq!(submit_calls, 2, "prepared then resubmitted once");
    assert_eq!(query_calls, 2, "unknown only reconciles the existing submission");
    assert!(matches!(plan, SubmissionPlan::Release { .. }));
}