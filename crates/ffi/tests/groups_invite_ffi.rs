//! GROUPS-019 邀请投递 FFI 用例（真 axum 节点 + 真 OpenMLS + 真临时加密库）。
//!
//! 覆盖：submit 拿到真回执（seq>=1、log_hash 64 hex、node_id 一致）→ fetch 回来的邀请与提交逐字节一致；
//! 重复 submit 同 submission_id；节点不可达 NODE_UNAVAILABLE；未配置端点 INVALID_TARGET；关闭后 CORE_CLOSED。

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chat_core::service::{ConversationCore, IdentityAccountInput, IdentityAccountView};
use chat_ffi::groups_ffi::{GroupFfi, InviteMetaFfi};
use chat_ffi::local_store::FfiError;
use chat_node_api::build_router;
use chat_node_api::state::{AppState, FixedClock as NodeFixedClock};
use chat_node_client::{NodeClient, NodeEndpoint};
use chat_node_services::canonical::hex_encode;
use chat_node_services::group::{GroupCreation, GroupMemberSpec};
use chat_node_services::registration::{
    DeviceCredential, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest, RegistrationCapability,
};
use chat_node_services::services::{NodeAuthPolicy, NodeService, NodeServiceConfig};
use ed25519_dalek::{Signature, Signer, SigningKey};

const NOW_S: u64 = 1_700_000_000;
const ACCOUNT: &str = "acct-ffi-invite";
const NODE_ID: &str = "node-ffi-invite";
const MASTER_KEY: [u8; 32] = [0x5a; 32];
const DEVICE_SEED: [u8; 32] = [0x33; 32];
const ACCOUNT_SEED: [u8; 32] = [0x22; 32];
const SUPERVISOR_SEED: [u8; 32] = [0x11; 32];
const UNREACHABLE_ENDPOINT: &str = "http://127.0.0.1:1";
const WELCOME_BYTES: [u8; 64] = [0xAB; 64];

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
            "chat-ffi-invite-{}-{nanos}-{counter}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create fixture dir");
        Self(dir)
    }

    fn database(&self) -> String {
        self.0.join("chat.db").to_string_lossy().into_owned()
    }
}

impl Drop for FixtureDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn master_key_hex() -> String {
    MASTER_KEY.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn device_key() -> SigningKey {
    SigningKey::from_bytes(&DEVICE_SEED)
}

fn supervisor_key() -> SigningKey {
    SigningKey::from_bytes(&SUPERVISOR_SEED)
}

fn acct(seed: &[u8; 32]) -> String {
    seed.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn enroll_identity(database: &str, master_key_hex: &str) -> IdentityAccountView {
    let mut core = ConversationCore::open(database, master_key_hex).expect("open core");
    let view = core
        .save_identity_account(IdentityAccountInput {
            display_name: "FFI 邀请投递".to_owned(),
            account_seed_hex: acct(&ACCOUNT_SEED),
            device_seed_hex: acct(&DEVICE_SEED),
            created_at_ms: NOW_S * 1_000,
        })
        .expect("save identity");
    core.close();
    view
}

fn code(error: FfiError) -> String {
    match error {
        FfiError::Store { code, .. } => code,
    }
}

fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(future)
}

struct TestNode {
    address: String,
    node_public_key: [u8; 32],
}

impl TestNode {
    fn endpoint_url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn public_key_hex(&self) -> String {
        hex_encode(&self.node_public_key)
    }

    fn client(&self, device_id: &str) -> NodeClient {
        let endpoint = NodeEndpoint::new(&self.endpoint_url(), NODE_ID)
            .expect("endpoint")
            .with_node_public_key(self.node_public_key);
        NodeClient::new(endpoint, device_id, device_key()).expect("node client")
    }

    fn registration_request(&self, device_id: &str) -> RegisterDeviceRequest {
        let supervisor = supervisor_key();
        let supervisor_public_key = hex_encode(&supervisor.verifying_key().to_bytes());
        let mut credential = DeviceCredential {
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&device_key().verifying_key().to_bytes()),
            endpoint_id: "endpoint-ffi-invite".into(),
            credential_version: 1,
            not_before: NOW_S - 10,
            expires_at: NOW_S + 3_600,
            supervisor_public_key: supervisor_public_key.clone(),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&credential.signing_bytes().expect("credential"));
        credential.signature = hex_encode(&signature.to_bytes());
        let mut capability = RegistrationCapability {
            capability_id: "cap-ffi-invite".into(),
            account_id: ACCOUNT.into(),
            device_id: device_id.to_owned(),
            purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
            issued_at: NOW_S - 10,
            expires_at: NOW_S + 600,
            revocation_version: 1,
            supervisor_public_key,
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&capability.signing_bytes().expect("capability"));
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
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
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
    }
}

struct Harness {
    /// 持有临时目录（Drop 时清理库文件）。
    _dir: FixtureDir,
    node: TestNode,
    ffi: Arc<GroupFfi>,
    group_id: String,
    device_id: String,
    meta: InviteMetaFfi,
}

impl Harness {
    fn new() -> Self {
        let dir = FixtureDir::new();
        let identity = enroll_identity(&dir.database(), &master_key_hex());
        let device_id = identity.device_id;

        let node = start_node();
        let client = node.client(&device_id);
        block_on(client.register_device(&node.registration_request(&device_id)))
            .expect("device registers on the node");

        let ffi = GroupFfi::open(dir.database(), master_key_hex(), Some(NODE_ID.to_owned()))
            .expect("open group ffi");
        let view = ffi
            .group_create(
                "FFI 邀请投递测试群".to_owned(),
                String::new(),
                "bob".to_owned(),
                Vec::new(),
            )
            .expect("create group");
        assert_eq!(view.members[0].credential, device_id);

        let creation = GroupCreation {
            group_id: view.group_id.clone(),
            owner_account_id: ACCOUNT.into(),
            initial_control_head: hex_encode(&[0_u8; 32]),
            policy_version: 1,
            mls_epoch: 0,
            members: vec![GroupMemberSpec {
                device_id: device_id.clone(),
                role: "owner".into(),
            }],
        };
        block_on(client.create_group(&creation)).expect("group registers on the node");

        let meta = InviteMetaFfi {
            group_id: view.group_id.clone(),
            mls_group_id: view.mls_group_key.clone(),
            name: view.name.clone(),
            inviter_credential: device_id.clone(),
            owner_credential: device_id.clone(),
            invitee_credential: "bob".to_owned(),
            members: vec![device_id.clone(), "bob".to_owned()],
            epoch: view.epoch,
        };
        Self {
            _dir: dir,
            node,
            ffi,
            group_id: view.group_id,
            device_id,
            meta,
        }
    }

    fn submit(&self) -> Result<chat_ffi::groups_ffi::InviteReceiptFfi, FfiError> {
        self.ffi.group_submit_invite(
            self.group_id.clone(),
            self.meta.clone(),
            WELCOME_BYTES.to_vec(),
            self.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(self.node.public_key_hex()),
        )
    }

    fn fetch(&self) -> Result<Option<chat_ffi::groups_ffi::GroupInviteBlobFfi>, FfiError> {
        self.fetch_for(None)
    }

    fn fetch_for(
        &self,
        invitee_credential: Option<&str>,
    ) -> Result<Option<chat_ffi::groups_ffi::GroupInviteBlobFfi>, FfiError> {
        self.ffi.group_fetch_invite(
            self.group_id.clone(),
            self.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(self.node.public_key_hex()),
            invitee_credential.map(str::to_owned),
        )
    }
}

#[test]
fn an_invite_is_submitted_and_fetched_back_through_the_node() {
    let harness = Harness::new();
    let receipt = harness.submit().expect("the node accepts the invite");
    assert!(!receipt.submission_id.is_empty());
    assert!(receipt.submission_id.starts_with("sub:1:"));
    assert!(receipt.seq >= 1, "node assigns a log sequence from 1");
    assert_eq!(receipt.log_hash.len(), 64, "log hash is 32 bytes of hex");
    assert!(receipt.log_hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(receipt.node_id, NODE_ID);

    // 取回：元数据与 Welcome 字节逐字节一致。
    let fetched = harness.fetch().expect("fetch").expect("the invite must be found");
    assert_eq!(fetched.meta, harness.meta);
    assert_eq!(fetched.welcome_bytes, WELCOME_BYTES.to_vec());
    assert_eq!(fetched.meta.group_id, harness.group_id);
    assert_eq!(fetched.meta.invitee_credential, "bob");
    assert_eq!(fetched.meta.inviter_credential, harness.device_id);

    // 重复提交：同一个提交标识与同一条日志位置（节点侧幂等 + 对账）。
    let again = harness.submit().expect("the duplicate is idempotent");
    assert_eq!(again.submission_id, receipt.submission_id);
    assert_eq!(again.seq, receipt.seq);
    assert_eq!(again.log_hash, receipt.log_hash);
}

#[test]
fn an_unreachable_or_unconfigured_node_is_explicit() {
    let harness = Harness::new();
    // 不可达：submit / fetch 都是 NODE_UNAVAILABLE（不会静默当成成功/没有邀请）。
    let error = harness
        .ffi
        .group_submit_invite(
            harness.group_id.clone(),
            harness.meta.clone(),
            WELCOME_BYTES.to_vec(),
            UNREACHABLE_ENDPOINT.to_owned(),
            NODE_ID.to_owned(),
            Some(harness.node.public_key_hex()),
        )
        .expect_err("unreachable node");
    assert_eq!(code(error), "NODE_UNAVAILABLE");
    let error = harness
        .ffi
        .group_fetch_invite(
            harness.group_id.clone(),
            UNREACHABLE_ENDPOINT.to_owned(),
            NODE_ID.to_owned(),
            Some(harness.node.public_key_hex()),
            None,
        )
        .expect_err("unreachable node on fetch");
    assert_eq!(code(error), "NODE_UNAVAILABLE");

    // 未配置端点 → INVALID_TARGET（显式，不回退）。
    let error = harness
        .ffi
        .group_submit_invite(
            harness.group_id.clone(),
            harness.meta.clone(),
            WELCOME_BYTES.to_vec(),
            String::new(),
            NODE_ID.to_owned(),
            None,
        )
        .expect_err("empty endpoint");
    assert_eq!(code(error), "INVALID_TARGET");
}

#[test]
fn an_unknown_group_is_reported_explicitly() {
    let harness = Harness::new();
    let unknown_group = format!("grp:1:{}", "7".repeat(64));
    let error = harness
        .ffi
        .group_fetch_invite(
            unknown_group,
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.node.public_key_hex()),
            None,
        )
        .expect_err("fetching an unknown group must fail explicitly");
    // 节点侧拒绝（群不存在）→ 稳定码不是 NODE_UNAVAILABLE 也不是成功。
    let unknown_code = code(error);
    assert_ne!(unknown_code, "NODE_UNAVAILABLE");
    assert!(!unknown_code.is_empty());
}


#[test]
fn invites_are_filtered_by_the_invited_credential() {
    let harness = Harness::new();
    // 两条邀请：bob（harness.meta）与 carol。
    let bob_meta = harness.meta.clone();
    let carol_meta = InviteMetaFfi {
        invitee_credential: "carol".to_owned(),
        members: vec![harness.device_id.clone(), "carol".to_owned()],
        // 同一个群、同一个 epoch，只是被邀请者不同。
        ..harness.meta.clone()
    };
    let bob_receipt = harness.submit().expect("bob invite submitted");
    let carol_receipt = harness
        .ffi
        .group_submit_invite(
            harness.group_id.clone(),
            carol_meta.clone(),
            vec![0xCD; 48],
            harness.node.endpoint_url(),
            NODE_ID.to_owned(),
            Some(harness.node.public_key_hex()),
        )
        .expect("carol invite submitted");
    assert_ne!(carol_receipt.submission_id, bob_receipt.submission_id);

    // 按 bob 过滤：只拿到 bob 的那条（即使 carol 是更晚提交的）。
    let only_bob = harness.fetch_for(Some("bob")).expect("fetch bob").expect("bob invite");
    assert_eq!(only_bob.meta, bob_meta);
    assert_eq!(only_bob.welcome_bytes, WELCOME_BYTES.to_vec());
    // 按 carol 过滤：只拿到 carol 的那条。
    let only_carol = harness.fetch_for(Some("carol")).expect("fetch carol").expect("carol invite");
    assert_eq!(only_carol.meta, carol_meta);
    assert_eq!(only_carol.welcome_bytes, vec![0xCD; 48]);
    // 不传过滤：最近的一条（carol 后提交）→ 旧行为不变。
    let latest = harness.fetch().expect("fetch unfiltered").expect("some invite");
    assert_eq!(latest.meta.invitee_credential, "carol");
    // 未知凭据：None，不报错。
    let none = harness.fetch_for(Some("dave")).expect("unknown invitee is not an error");
    assert!(none.is_none(), "somebody else's invite is not returned");
}
#[test]
fn a_closed_object_rejects_invite_calls() {
    let harness = Harness::new();
    let receipt = harness.submit().expect("submit before close");
    assert!(receipt.seq >= 1);
    harness.ffi.shutdown();
    assert_eq!(code(harness.submit().expect_err("closed submit")), "CORE_CLOSED");
    assert_eq!(code(harness.fetch().expect_err("closed fetch")), "CORE_CLOSED");
}
