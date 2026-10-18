//! SW 包端到端：真节点（axum + node-services）+ 真库，验证拉取/验签/落库/游标语义。
//!
//! 覆盖：追加→拉取并前进游标、重复调用幂等 0 应用、本地越过节点要求 Resync、
//! 验签失败拒绝且游标不动、节点不可达显式错误且游标不变、追赶计划执行 PullLog 后停在上层动作之前。
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::sync_worker::{
    initialize_pull_state, load_applied_entries, load_pull_state, run_catchup_once_at,
    run_pull_once_at, verify_pull_page,
};
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCandidate, GroupCreation, GroupMemberSpec, KIND_APPLICATION};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use chat_protocol::{ControlHead, DeviceId, Digest, GroupId, SubmissionId, VerifiedHead};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use chat_sync::catchup::{AwaitReason, CatchupStep};
use chat_sync::pull::ResyncReason;
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const NOW_MS: i64 = 1_700_000_000_000;
const ACCOUNT: &str = "acct-core-sw";
const NODE_ID: &str = "node-core-sw";
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const DEVICE_SEED: u8 = 31;
const INITIAL_CONTROL_HEAD: [u8; 32] = [0x11; 32];

fn device_id_text(seed: u8) -> String {
    DeviceId::from_bytes([seed; 32]).encode()
}

fn device_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn submission_text(seed: u8) -> String {
    SubmissionId::from_bytes([seed; 32]).encode()
}

/// 让异步客户端调用能在同步测试里跑（测试线程自己的运行时；节点在别的线程上）。
fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

// ---------------------------------------------------------------- 真实节点夹具

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
    supervisor: SigningKey,
}

impl TestNode {
    fn endpoint(&self) -> NodeEndpoint {
        NodeEndpoint::new(&format!("http://{}", self.address), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key)
    }

    fn client(&self, device_id: &str, key: &SigningKey) -> NodeClient {
        NodeClient::new(self.endpoint(), device_id, key.clone()).expect("node client")
    }

    fn registration_request(
        &self,
        device_id: &str,
        key: &SigningKey,
        capability_id: &str,
    ) -> RegisterDeviceRequest {
        let supervisor_public_key = hex_encode(&self.supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&key.verifying_key().to_bytes()),
            endpoint_id: "endpoint-sw".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&credential.signing_bytes().expect("credential bytes"));
        credential.signature = hex_encode(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: capability_id.to_owned(),
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: NOW_S - 10,
            expires_at: NOW_S + 600,
            revocation_version: 1,
            supervisor_public_key,
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&capability.signing_bytes().expect("capability bytes"));
        capability.signature = hex_encode(&signature.to_bytes());
        RegisterDeviceRequest {
            account_id: ACCOUNT.into(),
            credential,
            capability,
        }
    }
}

fn start_node() -> TestNode {
    let service = NodeService::open_in_memory(NodeServiceConfig {
        node_id: NODE_ID.into(),
        signing_seed: [7_u8; 32],
        policy: NodeAuthPolicy::default(),
    })
    .expect("node service opens");
    let node_public_key = service.node_public_key();
    let state = AppState::new(service, Arc::new(NodeFixedClock::new(NOW_S)));
    let router = build_router(state);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("nonblocking listener");
    let address = listener.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                return;
            };
            let _ = axum::serve(listener, router).await;
        });
    });
    TestNode {
        address,
        node_public_key,
        supervisor: SigningKey::from_bytes(&[11_u8; 32]),
    }
}

// ---------------------------------------------------------------- 存储夹具

struct FixtureDir(PathBuf);

impl FixtureDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "chat-core-sync-worker-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(self.0.join("chat.db"), KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(MASTER_KEY),
        )
        .expect("open formal store")
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------- 群夹具

struct Harness {
    dir: FixtureDir,
    store: Store,
    node: TestNode,
    client: NodeClient,
    group_id: String,
}

impl Harness {
    /// 起真节点、注册设备、登记一个空群（节点上还没有任何日志条目）。
    fn new() -> Self {
        let dir = FixtureDir::new();
        let store = dir.open();
        let node = start_node();
        let device_id = device_id_text(DEVICE_SEED);
        let key = device_key(DEVICE_SEED);
        let client = node.client(&device_id, &key);
        block_on(client.register_device(&node.registration_request(&device_id, &key, "cap-sw")))
            .expect("device registers");
        let group_id = GroupId::from_bytes([41_u8; 32]).encode();
        let creation = GroupCreation {
            group_id: group_id.clone(),
            owner_account_id: ACCOUNT.into(),
            initial_control_head: hex_encode(&INITIAL_CONTROL_HEAD),
            policy_version: 1,
            mls_epoch: 0,
            members: vec![GroupMemberSpec {
                device_id: device_id.clone(),
                role: "owner".into(),
            }],
        };
        block_on(client.create_group(&creation)).expect("group registers on the node");
        Self {
            dir,
            store,
            node,
            client,
            group_id,
        }
    }

    /// 创世起点（来自建群回执的已验证 head：序号 0、全零链尾、初始控制头）。
    fn genesis(&self) -> VerifiedHead {
        VerifiedHead::genesis(
            GroupId::parse(&self.group_id).expect("group id"),
            ControlHead::from_bytes(INITIAL_CONTROL_HEAD),
            1,
            0,
        )
    }

    fn initialize(&mut self) {
        let genesis = self.genesis();
        initialize_pull_state(&mut self.store, &self.group_id, genesis).expect("pull state");
    }

    /// 追加一条应用密文条目（作者是本机设备），返回节点分配的回执。
    fn append(&self, submission_seed: u8, payload: &[u8]) -> chat_node_services::group::AppendResult {
        let head = block_on(self.client.get_head(&self.group_id)).expect("node head");
        let candidate = GroupCandidate {
            protocol_major: 1,
            protocol_minor: 0,
            submission_id: submission_text(submission_seed),
            group_id: self.group_id.clone(),
            author_device_id: device_id_text(DEVICE_SEED),
            kind: KIND_APPLICATION.to_owned(),
            expected_control_head: head.control_head.clone(),
            policy_version: head.policy_version,
            mls_epoch: head.mls_epoch,
            encrypted_payload: hex_encode(payload),
            control_metadata: None,
        };
        block_on(self.client.append_entry(&candidate)).expect("entry accepted")
    }

    fn node_head_seq(&self) -> u64 {
        block_on(self.client.get_head(&self.group_id))
            .expect("node head")
            .seq
    }

    fn node_page(&self, after: u64, limit: u32) -> chat_node_services::group::LogPage {
        block_on(self.client.fetch_log(&self.group_id, after, limit)).expect("node page")
    }

    fn cursor(&self) -> u64 {
        load_pull_state(&self.store, &self.group_id)
            .expect("state")
            .expect("initialized")
            .cursor_seq
    }
}

// ---------------------------------------------------------------- 用例

#[test]
fn pull_applies_new_entries_and_advances_the_cursor() {
    let mut harness = Harness::new();
    harness.initialize();
    assert_eq!(harness.cursor(), 0);
    let first = harness.append(51, b"first ciphertext");
    let second = harness.append(52, b"second ciphertext");
    assert_eq!(first.seq, 1);
    assert_eq!(second.seq, 2);
    assert_eq!(harness.node_head_seq(), 2);

    let outcome = run_pull_once_at(&mut harness.store, &harness.client, &harness.group_id, NOW_MS)
        .expect("pull applies the entries");
    assert_eq!(outcome.applied, 2);
    assert_eq!(outcome.cursor_after, 2);
    assert!(!outcome.resync_required);
    assert_eq!(outcome.node_head, 2);
    assert_eq!(harness.cursor(), 2);

    // 条目真的落库了（内容与节点一致，按序号升序）。
    let entries = load_applied_entries(&harness.store, &harness.group_id).expect("entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].receipt.seq, 1);
    assert_eq!(entries[0].receipt.submission_id, submission_text(51));
    assert_eq!(entries[0].candidate.candidate.encrypted_payload, hex_encode(b"first ciphertext"));
    assert_eq!(entries[1].receipt.log_hash, second.log_hash);
    let state = load_pull_state(&harness.store, &harness.group_id)
        .expect("state")
        .expect("initialized");
    assert_eq!(state.applied_entries, 2);
    assert_eq!(state.cursor_log_hash, second.log_hash);
    assert_eq!(state.consecutive_failures, 0);
}

#[test]
fn repeated_pulls_are_idempotent_and_never_duplicate_entries() {
    let mut harness = Harness::new();
    harness.initialize();
    harness.append(53, b"only ciphertext");
    let first = run_pull_once_at(&mut harness.store, &harness.client, &harness.group_id, NOW_MS)
        .expect("first pull");
    assert_eq!(first.applied, 1);
    // 已到节点 head：第二轮 0 应用、只记录退避，不动游标与条目。
    let second = run_pull_once_at(
        &mut harness.store,
        &harness.client,
        &harness.group_id,
        NOW_MS + 10_000,
    )
    .expect("second pull");
    assert_eq!(second.applied, 0);
    assert_eq!(second.cursor_after, 1);
    assert!(second.backoff_ms.is_some(), "idle pull reports its backoff");
    assert_eq!(harness.cursor(), 1);
    assert_eq!(
        load_applied_entries(&harness.store, &harness.group_id)
            .expect("entries")
            .len(),
        1
    );
    let state = load_pull_state(&harness.store, &harness.group_id)
        .expect("state")
        .expect("initialized");
    assert_eq!(state.applied_entries, 1, "no duplicate application");
}

#[test]
fn local_cursor_ahead_of_the_node_requires_resync() {
    let mut harness = Harness::new();
    // 本地声称已应用到 seq 3，但节点只有 1 条（换节点/节点回退）。
    let ahead = VerifiedHead {
        group_id: GroupId::parse(&harness.group_id).expect("group"),
        control_head: ControlHead::from_bytes(INITIAL_CONTROL_HEAD),
        policy_version: 1,
        mls_epoch: 0,
        log_sequence: 3,
        log_hash: Digest::from_bytes([0x33; 32]),
    };
    initialize_pull_state(&mut harness.store, &harness.group_id, ahead).expect("state");
    harness.append(54, b"node entry");
    let outcome = run_pull_once_at(&mut harness.store, &harness.client, &harness.group_id, NOW_MS)
        .expect("decision core answers with resync");
    assert_eq!(outcome.applied, 0);
    assert!(outcome.resync_required);
    assert_eq!(outcome.resync_reason, Some(ResyncReason::LocalAheadOfNode));
    assert_eq!(outcome.cursor_after, 3, "cursor is never rolled back silently");
    assert!(load_applied_entries(&harness.store, &harness.group_id)
        .expect("entries")
        .is_empty());
}

#[test]
fn tampered_or_gapped_pages_are_rejected_before_anything_is_stored() {
    let mut harness = Harness::new();
    harness.initialize();
    harness.append(55, b"first ciphertext");
    harness.append(56, b"second ciphertext");
    let page = harness.node_page(0, 10);
    assert_eq!(page.entries.len(), 2);
    let pinned = harness.node.node_public_key;
    let genesis_hash = Digest::ZERO;
    // 真实页可以验过（证明“校验不是永远失败”）。
    verify_pull_page(&pinned, &harness.group_id, genesis_hash, 1, 2, &page.entries)
        .expect("genuine page verifies");

    // 1) 换节点公钥：任何条目都不可信。
    let error = verify_pull_page(&[0x99; 32], &harness.group_id, genesis_hash, 1, 2, &page.entries)
        .expect_err("wrong pinned key");
    assert_eq!(error.code(), chat_protocol::ErrorCode::InvalidSignature);
    // 2) 链断裂（改前序摘要）。
    let mut broken = page.entries.clone();
    broken[1].receipt.previous_hash = "00".repeat(32);
    let error = verify_pull_page(&pinned, &harness.group_id, genesis_hash, 1, 2, &broken)
        .expect_err("broken chain");
    assert_eq!(error.code(), chat_protocol::ErrorCode::LogConflict);
    // 3) 候选被换掉（回执不再绑定该候选）。
    let mut swapped = page.entries.clone();
    swapped[0].candidate.candidate.encrypted_payload = hex_encode(b"another payload");
    let error = verify_pull_page(&pinned, &harness.group_id, genesis_hash, 1, 2, &swapped)
        .expect_err("candidate swapped");
    assert_eq!(error.code(), chat_protocol::ErrorCode::InvalidSignature);
    // 4) 页码对不上（缺口绝不静默跳过）。
    let error = verify_pull_page(&pinned, &harness.group_id, genesis_hash, 1, 3, &page.entries)
        .expect_err("page does not cover the range");
    assert_eq!(error.code(), chat_protocol::ErrorCode::MissingLogEntries);
    // 5) 起点不是游标的下一条（用一条条目的页配上错的前序摘要）。
    let later = page.entries[1].receipt.log_hash.clone();
    let mut hash = [0_u8; 32];
    for index in 0..32 {
        hash[index] = u8::from_str_radix(&later[index * 2..index * 2 + 2], 16).expect("hex");
    }
    let single = vec![page.entries[0].clone()];
    let error = verify_pull_page(&pinned, &harness.group_id, Digest::from_bytes(hash), 1, 1, &single)
        .expect_err("entry does not continue the verified chain");
    assert_eq!(error.code(), chat_protocol::ErrorCode::LogConflict);
}

#[test]
fn verification_failure_does_not_advance_the_cursor() {
    let mut harness = Harness::new();
    harness.initialize();
    harness.append(57, b"entry for the good node");
    // 同设备身份但固定了**错误**的节点公钥：挑战/回执验签必然失败。
    let wrong_key = NodeClient::new(
        NodeEndpoint::new(&format!("http://{}", harness.node.address), NODE_ID)
            .expect("endpoint")
            .with_node_public_key([0x42; 32]),
        device_id_text(DEVICE_SEED),
        device_key(DEVICE_SEED),
    )
    .expect("client");
    let error = run_pull_once_at(&mut harness.store, &wrong_key, &harness.group_id, NOW_MS)
        .expect_err("an untrusted node cannot be pulled from");
    assert!(
        matches!(
            error.code(),
            chat_protocol::ErrorCode::InvalidSignature | chat_protocol::ErrorCode::NodeUnavailable
        ),
        "unexpected code: {}",
        error.code().as_str()
    );
    // 游标与条目都没动；只记录失败退避（不 sleep）。
    let state = load_pull_state(&harness.store, &harness.group_id)
        .expect("state")
        .expect("initialized");
    assert_eq!(state.cursor_seq, 0);
    assert_eq!(state.applied_entries, 0);
    assert!(load_applied_entries(&harness.store, &harness.group_id)
        .expect("entries")
        .is_empty());
    // 真节点仍然能拉到。
    let outcome = run_pull_once_at(
        &mut harness.store,
        &harness.client,
        &harness.group_id,
        NOW_MS + 60_000,
    )
    .expect("the pinned node still works");
    assert_eq!(outcome.applied, 1);
}

#[test]
fn unreachable_node_fails_explicitly_and_backs_off_without_sleeping() {
    let mut harness = Harness::new();
    harness.initialize();
    harness.append(58, b"entry while offline");
    let unreachable = NodeClient::new(
        NodeEndpoint::new("http://127.0.0.1:1", NODE_ID)
            .expect("endpoint")
            .with_node_public_key(harness.node.node_public_key),
        device_id_text(DEVICE_SEED),
        device_key(DEVICE_SEED),
    )
    .expect("client");
    let error = run_pull_once_at(&mut harness.store, &unreachable, &harness.group_id, NOW_MS)
        .expect_err("unreachable node");
    assert_eq!(error.code(), chat_protocol::ErrorCode::NodeUnavailable);
    let state = load_pull_state(&harness.store, &harness.group_id)
        .expect("state")
        .expect("initialized");
    assert_eq!(state.cursor_seq, 0, "cursor never advances on failure");
    assert_eq!(state.consecutive_failures, 1, "failure is counted for backoff");
    assert!(state.next_attempt_at_ms > NOW_MS);
    // 退避窗口内再调用：不再打节点（得到 Backoff，0 应用），也不 sleep。
    let backoff = run_pull_once_at(
        &mut harness.store,
        &unreachable,
        &harness.group_id,
        NOW_MS + 1,
    )
    .expect("backoff decision");
    assert_eq!(backoff.applied, 0);
    assert!(backoff.backoff_ms.is_some());
    // 窗口过后重新尝试仍失败，失败计数继续累计（不无限重试，也不伪造成功）。
    let next_at = state.next_attempt_at_ms;
    let error = run_pull_once_at(&mut harness.store, &unreachable, &harness.group_id, next_at)
        .expect_err("still unreachable");
    assert_eq!(error.code(), chat_protocol::ErrorCode::NodeUnavailable);
    let state = load_pull_state(&harness.store, &harness.group_id)
        .expect("state")
        .expect("initialized");
    assert_eq!(state.consecutive_failures, 2);
}

#[test]
fn catchup_runs_pull_log_then_commits_the_head() {
    let mut harness = Harness::new();
    harness.initialize();
    harness.append(59, b"catch me one");
    harness.append(60, b"catch me two");
    harness.append(61, b"catch me three");
    let outcome = run_catchup_once_at(&mut harness.store, &harness.client, &harness.group_id, NOW_MS)
        .expect("catch-up runs");
    assert_eq!(outcome.applied, 3);
    assert_eq!(outcome.cursor_after, 3);
    assert!(outcome.executed_steps >= 2, "PullLog + CommitHead executed");
    assert!(outcome.upper_actions.is_empty(), "no upper action needed");
    assert_eq!(outcome.awaiting, None);
    assert!(!outcome.caught_up, "the plan had steps to run");
    assert_eq!(harness.cursor(), 3);
    // 再跑一次：计划已为空，0 应用。
    let again = run_catchup_once_at(&mut harness.store, &harness.client, &harness.group_id, NOW_MS)
        .expect("catch-up again");
    assert_eq!(again.applied, 0);
    assert!(again.caught_up);
    assert_eq!(again.executed_steps, 0);
}

#[test]
fn catchup_returns_upper_actions_instead_of_faking_epoch_progress() {
    let mut harness = Harness::new();
    // 本地策略版本比节点新（本机已提交控制结果，节点还没包含）：只能等待，不能推进。
    let ahead = VerifiedHead {
        group_id: GroupId::parse(&harness.group_id).expect("group"),
        control_head: ControlHead::from_bytes([0x44; 32]),
        policy_version: 2,
        mls_epoch: 0,
        log_sequence: 0,
        log_hash: Digest::ZERO,
    };
    initialize_pull_state(&mut harness.store, &harness.group_id, ahead).expect("state");
    harness.append(62, b"control not yet visible");
    let outcome = run_catchup_once_at(&mut harness.store, &harness.client, &harness.group_id, NOW_MS)
        .expect("plan is computed");
    assert_eq!(outcome.applied, 0, "nothing is applied while awaiting control");
    assert_eq!(outcome.cursor_after, 0);
    assert_eq!(outcome.awaiting, Some(AwaitReason::PendingControlResult));
    assert!(matches!(
        outcome.upper_actions.as_slice(),
        [CatchupStep::AwaitControl { .. }]
    ));
    assert_eq!(outcome.executed_steps, 0);
    assert!(!outcome.caught_up);
    // 本地状态没有被伪造推进。
    let state = load_pull_state(&harness.store, &harness.group_id)
        .expect("state")
        .expect("initialized");
    assert_eq!(state.cursor_seq, 0);
    assert_eq!(state.policy_version, 2);
    let _ = &harness.dir;
}
