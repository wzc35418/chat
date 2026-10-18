//! SYNC-008 追赶计划的验收用例（增量/分页/epoch 落后与跨越/等待控制结果/超限/幂等）。
use chat_protocol::{ControlHead, Digest, ErrorCode, GroupId, VerifiedHead};
use chat_sync::catchup::{
    AwaitReason, CatchupPolicy, CatchupStep, NodeLogHead, catchup_plan,
};

fn group_id() -> GroupId {
    GroupId::from_bytes([7_u8; 32])
}

fn control() -> ControlHead {
    ControlHead::from_bytes([1_u8; 32])
}

fn local(
    log_sequence: u64,
    mls_epoch: u64,
    policy_version: u64,
    control_head: ControlHead,
) -> VerifiedHead {
    VerifiedHead {
        group_id: group_id(),
        control_head,
        policy_version,
        mls_epoch,
        log_sequence,
        log_hash: Digest::from_bytes([2_u8; 32]),
    }
}

fn node(
    log_sequence: u64,
    mls_epoch: u64,
    policy_version: u64,
    control_head: ControlHead,
) -> NodeLogHead {
    NodeLogHead {
        log_sequence,
        log_hash: Digest::from_bytes([3_u8; 32]),
        control_head,
        policy_version,
        mls_epoch,
    }
}

#[test]
fn incremental_catch_up_pulls_the_gap_then_commits_the_head() {
    let plan = catchup_plan(
        local(40, 1, 3, control()),
        node(45, 1, 3, control()),
        CatchupPolicy::DEFAULT,
    )
    .expect("plan");
    assert_eq!(
        plan.steps,
        vec![
            CatchupStep::PullLog { from: 41, to: 45 },
            CatchupStep::CommitHead { log_sequence: 45, mls_epoch: 1 },
        ]
    );
    assert!(!plan.is_caught_up());
    assert_eq!(plan.entries_to_pull(), 5);
    assert!(plan.rebuild_targets().is_empty());
    assert_eq!(plan.target_log_sequence, 45);
}

#[test]
fn long_gaps_are_paged_and_each_step_stays_within_the_page_limit() {
    let policy = CatchupPolicy::DEFAULT;
    let plan = catchup_plan(local(0, 1, 1, control()), node(250, 1, 1, control()), policy)
        .expect("plan");
    let pages: Vec<(u64, u64)> = plan
        .steps
        .iter()
        .filter_map(|step| match step {
            CatchupStep::PullLog { from, to } => Some((*from, *to)),
            _ => None,
        })
        .collect();
    assert_eq!(pages, vec![(1, 100), (101, 200), (201, 250)]);
    // 范围连续、递增、无重叠也不留洞。
    for window in pages.windows(2) {
        assert_eq!(window[0].1 + 1, window[1].0);
    }
    for (from, to) in &pages {
        assert!(to - from < u64::from(policy.page_items));
    }
    assert_eq!(plan.entries_to_pull(), 250);
    assert_eq!(
        plan.steps.last(),
        Some(&CatchupStep::CommitHead { log_sequence: 250, mls_epoch: 1 })
    );
}

#[test]
fn epoch_lag_is_rebuilt_one_epoch_at_a_time_never_jumping() {
    let plan = catchup_plan(
        local(10, 1, 2, control()),
        node(12, 4, 2, control()),
        CatchupPolicy::DEFAULT,
    )
    .expect("plan");
    assert_eq!(plan.rebuild_targets(), vec![2, 3, 4]);
    let rebuilds: Vec<(u64, u64, u64)> = plan
        .steps
        .iter()
        .filter_map(|step| match step {
            CatchupStep::RebuildEpoch { at_sequence, from_epoch, to_epoch } => {
                Some((*at_sequence, *from_epoch, *to_epoch))
            }
            _ => None,
        })
        .collect();
    assert_eq!(rebuilds, vec![(12, 1, 2), (12, 2, 3), (12, 3, 4)]);
    // 拉取必须排在任何重建之前，最后才提交 head。
    let pull_index = plan
        .steps
        .iter()
        .position(|step| matches!(step, CatchupStep::PullLog { .. }))
        .unwrap();
    let first_rebuild = plan
        .steps
        .iter()
        .position(|step| matches!(step, CatchupStep::RebuildEpoch { .. }))
        .unwrap();
    let commit = plan
        .steps
        .iter()
        .position(|step| matches!(step, CatchupStep::CommitHead { .. }))
        .unwrap();
    assert!(pull_index < first_rebuild && first_rebuild < commit);
}

#[test]
fn newer_epoch_without_entries_must_wait_for_the_control_result() {
    // 节点 head 声称 epoch 5，但没有任何新条目承载该控制结果：不猜重建，只等待。
    let plan = catchup_plan(
        local(20, 1, 4, control()),
        node(20, 5, 4, ControlHead::from_bytes([9_u8; 32])),
        CatchupPolicy::DEFAULT,
    )
    .expect("plan");
    assert_eq!(
        plan.steps,
        vec![CatchupStep::AwaitControl {
            expected_control_head: ControlHead::from_bytes([9_u8; 32]),
            expected_policy_version: 4,
            reason: AwaitReason::MissingEpochCommit,
        }]
    );
    assert_eq!(plan.awaiting(), Some(AwaitReason::MissingEpochCommit));
    assert!(!plan.is_caught_up());
    assert!(plan.rebuild_targets().is_empty());

    // 控制头不同、epoch 也不同但同样没有条目：仍然只等待。
    let plan = catchup_plan(
        local(20, 2, 4, control()),
        node(20, 3, 4, ControlHead::from_bytes([9_u8; 32])),
        CatchupPolicy::DEFAULT,
    )
    .expect("plan");
    assert_eq!(plan.awaiting(), Some(AwaitReason::MissingEpochCommit));
}

#[test]
fn local_policy_ahead_of_the_node_waits_instead_of_rolling_back() {
    let plan = catchup_plan(
        local(20, 2, 7, ControlHead::from_bytes([5_u8; 32])),
        node(22, 2, 6, ControlHead::from_bytes([5_u8; 32])),
        CatchupPolicy::DEFAULT,
    )
    .expect("plan");
    assert_eq!(
        plan.steps,
        vec![CatchupStep::AwaitControl {
            expected_control_head: ControlHead::from_bytes([5_u8; 32]),
            expected_policy_version: 7,
            reason: AwaitReason::PendingControlResult,
        }]
    );
    assert_eq!(plan.awaiting(), Some(AwaitReason::PendingControlResult));
}

#[test]
fn conflicts_fail_explicitly_and_do_not_block_other_groups() {
    // 本地序号超过节点 head：LOG_CONFLICT，而不是“拉负数范围”。
    let error = catchup_plan(
        local(30, 1, 1, control()),
        node(20, 1, 1, control()),
        CatchupPolicy::DEFAULT,
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::LogConflict);
    assert!(error.detail().contains("log sequence"));

    // 本地 epoch 超过节点：同样是冲突。
    let error = catchup_plan(
        local(20, 3, 1, control()),
        node(20, 1, 1, control()),
        CatchupPolicy::DEFAULT,
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::LogConflict);

    // 另一个群不受影响（纯函数、每群独立计划）。
    let other_group = catchup_plan(
        local(0, 1, 1, control()),
        node(3, 1, 1, control()),
        CatchupPolicy::DEFAULT,
    )
    .expect("other group plans normally");
    assert_eq!(other_group.entries_to_pull(), 3);
}

#[test]
fn oversized_plans_are_rejected_instead_of_truncated() {
    let mut policy = CatchupPolicy::DEFAULT;
    policy.max_entries = 100;
    // 跨度超过条目上限：显式失败。
    let error = catchup_plan(local(0, 1, 1, control()), node(150, 1, 1, control()), policy)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ContentTooLarge);

    // 跨 epoch 数超过上限：缺控制日志，显式失败。
    let mut epoch_policy = CatchupPolicy::DEFAULT;
    epoch_policy.max_epoch_jumps = 2;
    let error = catchup_plan(
        local(0, 1, 1, control()),
        node(5, 9, 1, control()),
        epoch_policy,
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::MissingLogEntries);

    // 步数超过上限：显式失败（页数 + 重建 + 提交 head）。
    let mut step_policy = CatchupPolicy::DEFAULT;
    step_policy.page_items = 1;
    step_policy.max_steps = 4;
    let error = catchup_plan(local(0, 1, 1, control()), node(10, 1, 1, control()), step_policy)
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ContentTooLarge);

    // 策略本身越界：INVALID_TARGET。
    let mut bad = CatchupPolicy::DEFAULT;
    bad.page_items = 0;
    assert_eq!(
        catchup_plan(local(0, 1, 1, control()), node(1, 1, 1, control()), bad)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
    let mut bad_entries = CatchupPolicy::DEFAULT;
    bad_entries.max_entries = 1;
    assert_eq!(
        catchup_plan(local(0, 1, 1, control()), node(1, 1, 1, control()), bad_entries)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
    let mut bad_steps = CatchupPolicy::DEFAULT;
    bad_steps.max_steps = 1;
    assert_eq!(
        catchup_plan(local(0, 1, 1, control()), node(1, 1, 1, control()), bad_steps)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
    let mut bad_jumps = CatchupPolicy::DEFAULT;
    bad_jumps.max_epoch_jumps = 0;
    assert_eq!(
        catchup_plan(local(0, 1, 1, control()), node(1, 1, 1, control()), bad_jumps)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );

    // 序号超出可存储范围：INVALID_TARGET，而不是溢出。
    let mut huge = node(1, 1, 1, control());
    huge.log_sequence = chat_protocol::MAX_CURSOR_SEQ + 1;
    assert_eq!(
        catchup_plan(local(0, 1, 1, control()), huge, CatchupPolicy::DEFAULT)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidTarget
    );
}

#[test]
fn already_caught_up_is_an_idempotent_empty_plan() {
    let local_head = local(42, 3, 5, control());
    let node_head = node(42, 3, 5, control());
    let first = catchup_plan(local_head, node_head, CatchupPolicy::DEFAULT).expect("plan");
    let second = catchup_plan(local_head, node_head, CatchupPolicy::DEFAULT).expect("plan");
    assert!(first.is_caught_up());
    assert_eq!(first, second);
    assert!(first.steps.is_empty());
    assert_eq!(first.entries_to_pull(), 0);
    assert_eq!(first.awaiting(), None);
    // 重复规划不会产生“再提交一次 head”的多余步骤。
    assert_eq!(first.step_count(), 0);
}

#[test]
fn control_change_with_new_entries_is_explained_by_the_pulled_range() {
    // epoch 不变、控制头变化，但有新条目承载：正常拉取并提交 head（不额外等待）。
    let plan = catchup_plan(
        local(10, 2, 4, control()),
        node(12, 2, 5, ControlHead::from_bytes([4_u8; 32])),
        CatchupPolicy::DEFAULT,
    )
    .expect("plan");
    assert_eq!(
        plan.steps,
        vec![
            CatchupStep::PullLog { from: 11, to: 12 },
            CatchupStep::CommitHead { log_sequence: 12, mls_epoch: 2 },
        ]
    );
    assert_eq!(plan.target_policy_version, 5);
    assert_eq!(plan.awaiting(), None);
}
