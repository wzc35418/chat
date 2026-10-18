//! NODE-001 设备注册与 API-002/API-003 挑战鉴权的公开接口测试。
//!
//! 覆盖：正确签名通过、错误密钥拒绝、挑战重放拒绝、跨方法/路径/查询/正文/节点绑定拒绝、
//! 过期拒绝、设备上限、重复注册幂等、主管钥更换拒绝、能力作用域/期限、畸形与超大输入有界拒绝。
use chat_node_services::auth::{
    AuthChallenge, ChallengePurpose, RequestMeta, SignedAuth, request_signing_bytes,
    verify_auth_challenge,
};
use chat_node_services::canonical::{hex_decode, hex_encode, sha256};
use chat_node_services::error::NodeError;
use chat_node_services::registration::{
    DeviceCredential, DeviceRegistration, REGISTRATION_CAPABILITY_PURPOSE, RegisterDeviceRequest,
    RegistrationCapability,
};
use chat_node_services::services::{
    AuthenticatedDevice, NodeAuthPolicy, NodeService, NodeServiceConfig,
};
use ed25519_dalek::{Signature, Signer, SigningKey};

const START: u64 = 1_700_000_000;

struct Harness {
    service: NodeService,
    supervisor: SigningKey,
    account_id: String,
}

/// 一次完整的注册提交：请求正文、鉴权材料、签名覆盖的方法/路径与正文摘要。
struct SignedCall {
    request: RegisterDeviceRequest,
    auth: SignedAuth,
    meta: RequestMeta,
    body_hash: [u8; 32],
}

impl Harness {
    /// 使用节点默认策略（每账户 2 台设备，与产品口径一致）。
    fn new() -> Self {
        Self::with_policy(NodeAuthPolicy::default())
    }

    /// 使用显式设备上限，用来证明上限可配且仍被硬性执行。
    fn with_capacity(max_devices: usize) -> Self {
        Self::with_policy(NodeAuthPolicy {
            max_devices_per_account: max_devices,
            ..NodeAuthPolicy::default()
        })
    }

    fn with_policy(policy: NodeAuthPolicy) -> Self {
        let config = NodeServiceConfig {
            node_id: "node-test-1".into(),
            signing_seed: [7_u8; 32],
            policy,
        };
        Self {
            service: NodeService::open_in_memory(config).expect("node service opens"),
            supervisor: SigningKey::from_bytes(&[11_u8; 32]),
            account_id: "acct-test-1".into(),
        }
    }

    fn credential(
        &self,
        device_id: &str,
        device: &SigningKey,
        version: u64,
        not_before: u64,
        expires_at: u64,
    ) -> DeviceCredential {
        let mut credential = DeviceCredential {
            account_id: self.account_id.clone(),
            device_id: device_id.to_owned(),
            device_public_key: hex_encode(&device.verifying_key().to_bytes()),
            endpoint_id: format!("endpoint-{device_id}"),
            credential_version: version,
            not_before,
            expires_at,
            supervisor_public_key: hex_encode(&self.supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = self
            .supervisor
            .sign(&credential.signing_bytes().expect("credential bytes"));
        credential.signature = hex_encode(&signature.to_bytes());
        credential
    }

    fn capability_with(
        &self,
        supervisor: &SigningKey,
        capability_id: &str,
        device_id: &str,
        issued_at: u64,
        expires_at: u64,
        purpose: &str,
    ) -> RegistrationCapability {
        let mut capability = RegistrationCapability {
            capability_id: capability_id.to_owned(),
            account_id: self.account_id.clone(),
            device_id: device_id.to_owned(),
            purpose: purpose.to_owned(),
            issued_at,
            expires_at,
            revocation_version: 1,
            supervisor_public_key: hex_encode(&supervisor.verifying_key().to_bytes()),
            signature: String::new(),
        };
        let signature: Signature = supervisor.sign(&capability.signing_bytes().expect("bytes"));
        capability.signature = hex_encode(&signature.to_bytes());
        capability
    }

    fn capability(
        &self,
        capability_id: &str,
        device_id: &str,
        issued_at: u64,
        expires_at: u64,
    ) -> RegistrationCapability {
        self.capability_with(
            &self.supervisor,
            capability_id,
            device_id,
            issued_at,
            expires_at,
            REGISTRATION_CAPABILITY_PURPOSE,
        )
    }

    fn request(&self, device_id: &str, device: &SigningKey, capability_id: &str, now: u64) -> RegisterDeviceRequest {
        RegisterDeviceRequest {
            account_id: self.account_id.clone(),
            credential: self.credential(device_id, device, 1, now - 10, now + 3_600),
            capability: self.capability(capability_id, device_id, now - 10, now + 600),
        }
    }

    /// 签发挑战并用设备私钥签名一次请求。允许指定“错误”的方法/路径/查询/节点/正文摘要/用途，
    /// 用来证明签名原文确实绑定了这些字段。
    fn sign_auth(
        &mut self,
        device_id: &str,
        device: &SigningKey,
        method: &str,
        path: &str,
        query: &str,
        node_id: Option<&str>,
        body_hash: [u8; 32],
        purpose: ChallengePurpose,
        now: u64,
    ) -> (AuthChallenge, SignedAuth, RequestMeta, [u8; 32]) {
        let challenge = self
            .service
            .issue_auth_challenge(purpose, now)
            .expect("challenge issued");
        let nonce = hex_decode::<32>("nonce", &challenge.nonce).expect("nonce hex");
        let meta = RequestMeta::canonical(method, path, query).expect("canonical meta");
        let signed = request_signing_bytes(
            device_id,
            &meta,
            &body_hash,
            &nonce,
            node_id.unwrap_or(self.service.node_id()),
            purpose,
        );
        let signature: Signature = device.sign(&signed);
        let auth = SignedAuth {
            challenge_id: challenge.challenge_id.clone(),
            device_id: device_id.to_owned(),
            nonce: challenge.nonce.clone(),
            signature: hex_encode(&signature.to_bytes()),
        };
        (challenge, auth, meta, body_hash)
    }

    fn build_call(&mut self, device_id: &str, device: &SigningKey, capability_id: &str, now: u64) -> SignedCall {
        let request = self.request(device_id, device, capability_id, now);
        let hash = body_hash(&request);
        let (_, auth, meta, body_hash) = self.sign_auth(
            device_id,
            device,
            "POST",
            "/v1/devices",
            "",
            None,
            hash,
            ChallengePurpose::DeviceRegistration,
            now,
        );
        SignedCall { request, auth, meta, body_hash }
    }

    /// 用正确的绑定（POST /v1/devices、本节点、给定正文）提交注册。
    fn submit_request(
        &mut self,
        request: &RegisterDeviceRequest,
        device: &SigningKey,
        now: u64,
    ) -> Result<DeviceRegistration, NodeError> {
        let hash = body_hash(request);
        let device_id = request.credential.device_id.clone();
        let device_key = request.credential.device_key().expect("device key");
        let (_, auth, meta, body_hash) = self.sign_auth(
            &device_id,
            device,
            "POST",
            "/v1/devices",
            "",
            None,
            hash,
            ChallengePurpose::DeviceRegistration,
            now,
        );
        let proof = self.service.verify_device_signature(
            &meta,
            &body_hash,
            &auth,
            &device_key,
            ChallengePurpose::DeviceRegistration,
            now,
        )?;
        self.service.register_device(request, &proof, now)
    }

    fn submit_signed(&mut self, call: &SignedCall, now: u64) -> Result<DeviceRegistration, NodeError> {
        let device_key = call.request.credential.device_key().expect("device key");
        let proof = self.service.verify_device_signature(
            &call.meta,
            &call.body_hash,
            &call.auth,
            &device_key,
            ChallengePurpose::DeviceRegistration,
            now,
        )?;
        self.service.register_device(&call.request, &proof, now)
    }

    fn register(
        &mut self,
        device_id: &str,
        device: &SigningKey,
        capability_id: &str,
        now: u64,
    ) -> Result<DeviceRegistration, NodeError> {
        let call = self.build_call(device_id, device, capability_id, now);
        self.submit_signed(&call, now)
    }
}

fn body_hash(request: &RegisterDeviceRequest) -> [u8; 32] {
    sha256(&serde_json::to_vec(request).expect("request json"))
}

fn device(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

#[test]
fn valid_registration_returns_signed_receipt_and_persists_device() {
    let mut harness = Harness::new();
    let telephone = device(21);
    let challenge = harness
        .service
        .issue_auth_challenge(ChallengePurpose::DeviceRegistration, START)
        .expect("challenge");
    verify_auth_challenge(&challenge, &harness.service.node_public_key())
        .expect("challenge signed by node");
    assert_eq!(challenge.node_id, "node-test-1");
    assert_eq!(challenge.issued_at, START);
    assert_eq!(
        challenge.expires_at,
        START + harness.service.policy().challenge_ttl_seconds
    );

    let call = harness.build_call("dev-1", &telephone, "cap-1", START);
    let registration = harness.submit_signed(&call, START).expect("registration accepted");
    registration
        .verify(&harness.service.node_public_key())
        .expect("node receipt verifies");
    assert_eq!(registration.account_id, "acct-test-1");
    assert_eq!(registration.device_id, "dev-1");
    assert_eq!(registration.node_id, "node-test-1");
    assert_eq!(registration.credential_version, 1);
    assert_eq!(registration.registered_at, START);

    let stored = harness.service.device("dev-1").expect("query").expect("device exists");
    assert_eq!(stored.state, "active");
    assert_eq!(stored.account_id, "acct-test-1");
    assert_eq!(
        stored.device_public_key,
        hex_encode(&telephone.verifying_key().to_bytes())
    );
    assert_eq!(harness.service.active_device_count("acct-test-1").unwrap(), 1);
}

#[test]
fn wrong_device_key_is_rejected_without_consuming_the_challenge() {
    let mut harness = Harness::new();
    let genuine = device(21);
    let attacker = device(22);
    let call = harness.build_call("dev-1", &genuine, "cap-1", START);
    let public_key = call.request.credential.device_key().unwrap();
    // 攻击者用自己的私钥签名，请求里携带的却是真设备的凭据公钥。
    let (_, wrong_auth, meta, hash) = harness.sign_auth(
        "dev-1",
        &attacker,
        "POST",
        "/v1/devices",
        "",
        None,
        call.body_hash,
        ChallengePurpose::DeviceRegistration,
        START,
    );
    let error = harness
        .service
        .verify_device_signature(
            &meta,
            &hash,
            &wrong_auth,
            &public_key,
            ChallengePurpose::DeviceRegistration,
            START,
        )
        .expect_err("wrong signer rejected");
    assert_eq!(error.code(), "INVALID_SIGNATURE");
    // 失败签名没有消费挑战：正确签名仍然通过。
    assert!(harness.submit_signed(&call, START).is_ok());
}

#[test]
fn second_use_of_the_same_challenge_is_rejected() {
    let mut harness = Harness::new();
    let telephone = device(21);
    let call = harness.build_call("dev-1", &telephone, "cap-1", START);
    harness.submit_signed(&call, START).expect("first use accepted");
    let error = harness.submit_signed(&call, START).expect_err("replay rejected");
    assert_eq!(error.code(), "AUTH_REPLAY");
}

#[test]
fn signature_is_bound_to_method_path_query_body_and_node() {
    let mut harness = Harness::new();
    let telephone = device(21);
    let request = harness.request("dev-1", &telephone, "cap-1", START);
    let hash = body_hash(&request);
    let public_key = request.credential.device_key().unwrap();
    let variants: [(&str, &str, &str, Option<&str>, [u8; 32]); 5] = [
        ("GET", "/v1/devices", "", None, hash),
        ("POST", "/v1/auth/challenge", "", None, hash),
        ("POST", "/v1/devices", "x=1", None, hash),
        ("POST", "/v1/devices", "", Some("node-other"), hash),
        ("POST", "/v1/devices", "", None, sha256(b"different body")),
    ];
    // 验签侧始终使用真实请求绑定（POST /v1/devices，无查询，本节点，真实正文摘要）。
    let canonical_meta = RequestMeta::canonical("POST", "/v1/devices", "").unwrap();
    for (method, path, query, node_id, variant_hash) in variants {
        let (_, auth, _, signed_hash) = harness.sign_auth(
            "dev-1",
            &telephone,
            method,
            path,
            query,
            node_id,
            variant_hash,
            ChallengePurpose::DeviceRegistration,
            START,
        );
        let error = harness
            .service
            .verify_device_signature(
                &canonical_meta,
                &hash,
                &auth,
                &public_key,
                ChallengePurpose::DeviceRegistration,
                START,
            )
            .expect_err("cross-bound signature must fail");
        assert_eq!(
            error.code(),
            "INVALID_SIGNATURE",
            "variant {method} {path} {query} node={node_id:?} hash={signed_hash:?}"
        );
    }
    // 完全正确的绑定仍可通过（证明失败来自绑定而非其它状态）。
    let (_, auth, _, signed_hash) = harness.sign_auth(
        "dev-1",
        &telephone,
        "POST",
        "/v1/devices",
        "",
        None,
        hash,
        ChallengePurpose::DeviceRegistration,
        START,
    );
    let proof = harness
        .service
        .verify_device_signature(
            &canonical_meta,
            &signed_hash,
            &auth,
            &public_key,
            ChallengePurpose::DeviceRegistration,
            START,
        )
        .expect("correct binding accepted");
    harness
        .service
        .register_device(&request, &proof, START)
        .expect("registration accepted");
}

#[test]
fn expired_challenge_is_rejected() {
    let mut harness = Harness::new();
    let telephone = device(21);
    let call = harness.build_call("dev-1", &telephone, "cap-1", START);
    let ttl = harness.service.policy().challenge_ttl_seconds;
    let error = harness.submit_signed(&call, START + ttl).expect_err("expired rejected");
    assert_eq!(error.code(), "AUTH_EXPIRED");
}

#[test]
fn purpose_mismatch_is_rejected() {
    let mut harness = Harness::new();
    let telephone = device(21);
    let request = harness.request("dev-1", &telephone, "cap-1", START);
    let hash = body_hash(&request);
    let public_key = request.credential.device_key().unwrap();
    let (_, auth, meta, signed_hash) = harness.sign_auth(
        "dev-1",
        &telephone,
        "POST",
        "/v1/devices",
        "",
        None,
        hash,
        ChallengePurpose::RequestAuth,
        START,
    );
    let error = harness
        .service
        .verify_device_signature(
            &meta,
            &signed_hash,
            &auth,
            &public_key,
            ChallengePurpose::DeviceRegistration,
            START,
        )
        .expect_err("purpose mismatch rejected");
    assert_eq!(error.code(), "INVALID_CHALLENGE");
}

#[test]
fn duplicate_registration_is_idempotent_and_does_not_add_devices() {
    let mut harness = Harness::new();
    let telephone = device(21);
    let first = harness
        .register("dev-1", &telephone, "cap-1", START)
        .expect("first registration");
    // 重复注册必须用新挑战，但请求正文与注册能力完全相同。
    let call = harness.build_call("dev-1", &telephone, "cap-1", START);
    let second = harness.submit_signed(&call, START).expect("idempotent repeat");
    assert_eq!(first, second);
    assert_eq!(harness.service.active_device_count("acct-test-1").unwrap(), 1);
}

#[test]
fn same_device_with_different_credential_is_rejected() {
    let mut harness = Harness::new();
    let telephone = device(21);
    harness
        .register("dev-1", &telephone, "cap-1", START)
        .expect("first registration");
    let hijacker = device(23);
    let mut request = harness.request("dev-1", &hijacker, "cap-2", START);
    request.credential.endpoint_id = "endpoint-hijack".into();
    let signature: Signature = harness
        .supervisor
        .sign(&request.credential.signing_bytes().unwrap());
    request.credential.signature = hex_encode(&signature.to_bytes());
    let call = SignedCall {
        auth: harness
            .sign_auth(
                "dev-1",
                &hijacker,
                "POST",
                "/v1/devices",
                "",
                None,
                body_hash(&request),
                ChallengePurpose::DeviceRegistration,
                START,
            )
            .1,
        meta: RequestMeta::canonical("POST", "/v1/devices", "").unwrap(),
        body_hash: body_hash(&request),
        request,
    };
    let error = harness.submit_signed(&call, START).expect_err("credential replacement rejected");
    assert_eq!(error.code(), "INVALID_CREDENTIAL");
}

#[test]
fn supervisor_key_change_is_rejected() {
    let mut harness = Harness::new();
    let telephone = device(21);
    harness
        .register("dev-1", &telephone, "cap-1", START)
        .expect("first registration");
    let attacker_supervisor = device(31);
    let new_device = device(25);
    let mut request = harness.request("dev-2", &new_device, "cap-2", START);
    request.credential.supervisor_public_key =
        hex_encode(&attacker_supervisor.verifying_key().to_bytes());
    let signature: Signature = attacker_supervisor
        .sign(&request.credential.signing_bytes().unwrap());
    request.credential.signature = hex_encode(&signature.to_bytes());
    request.capability = harness.capability_with(
        &attacker_supervisor,
        "cap-2",
        "dev-2",
        START - 10,
        START + 600,
        REGISTRATION_CAPABILITY_PURPOSE,
    );
    let call = SignedCall {
        auth: harness
            .sign_auth(
                "dev-2",
                &new_device,
                "POST",
                "/v1/devices",
                "",
                None,
                body_hash(&request),
                ChallengePurpose::DeviceRegistration,
                START,
            )
            .1,
        meta: RequestMeta::canonical("POST", "/v1/devices", "").unwrap(),
        body_hash: body_hash(&request),
        request,
    };
    let error = harness.submit_signed(&call, START).expect_err("supervisor change rejected");
    assert_eq!(error.code(), "INVALID_CREDENTIAL");
    assert_eq!(harness.service.active_device_count("acct-test-1").unwrap(), 1);
}

#[test]
fn device_limit_rejects_registrations_beyond_the_cap() {
    // 默认策略：每账户 2 台设备（产品口径），第三台明确拒绝。
    let mut default_policy = Harness::new();
    assert_eq!(default_policy.service.policy().max_devices_per_account, 2);
    assert!(default_policy.register("dev-0", &device(41), "cap-0", START).is_ok());
    assert!(default_policy.register("dev-1", &device(42), "cap-1", START).is_ok());
    let error = default_policy
        .register("dev-2", &device(43), "cap-2", START)
        .expect_err("third device rejected under the default cap");
    assert_eq!(error.code(), "DEVICE_LIMIT_EXCEEDED");
    assert_eq!(default_policy.service.active_device_count("acct-test-1").unwrap(), 2);
    // 被拒绝的设备没有落库。
    assert!(default_policy.service.device("dev-2").unwrap().is_none());

    // 上限可配：同一服务在 cap=3 时接受三台，第四台仍明确拒绝。
    let mut configured = Harness::with_capacity(3);
    for (index, seed) in [21_u8, 22, 23].into_iter().enumerate() {
        configured
            .register(&format!("dev-{index}"), &device(seed), &format!("cap-{index}"), START)
            .expect("within configured limit");
    }
    assert_eq!(configured.service.active_device_count("acct-test-1").unwrap(), 3);
    let error = configured
        .register("dev-3", &device(24), "cap-3", START)
        .expect_err("fourth device rejected");
    assert_eq!(error.code(), "DEVICE_LIMIT_EXCEEDED");
    assert_eq!(configured.service.active_device_count("acct-test-1").unwrap(), 3);
}

#[test]
fn capability_scope_expiry_and_purpose_are_enforced() {
    let mut harness = Harness::new();
    let telephone = device(21);

    // 作用域指向另一台设备。
    let mut request = harness.request("dev-1", &telephone, "cap-1", START);
    request.capability = harness.capability("cap-1", "dev-2", START - 10, START + 600);
    assert_eq!(
        harness.submit_request(&request, &telephone, START).unwrap_err().code(),
        "INVALID_CAPABILITY"
    );

    // 已过期的能力。
    let mut request = harness.request("dev-1", &telephone, "cap-2", START);
    request.capability = harness.capability("cap-2", "dev-1", START - 600, START - 1);
    assert_eq!(
        harness.submit_request(&request, &telephone, START).unwrap_err().code(),
        "INVALID_CAPABILITY"
    );

    // 作用域字符串不是设备注册。
    let mut request = harness.request("dev-1", &telephone, "cap-3", START);
    request.capability = harness.capability_with(
        &harness.supervisor.clone(),
        "cap-3",
        "dev-1",
        START - 10,
        START + 600,
        "group_admin",
    );
    assert_eq!(
        harness.submit_request(&request, &telephone, START).unwrap_err().code(),
        "INVALID_CAPABILITY"
    );

    // 能力签名被篡改（换设备标识后不重签）。
    let mut request = harness.request("dev-1", &telephone, "cap-4", START);
    request.capability.device_id = "dev-9".to_string();
    request.capability.purpose = REGISTRATION_CAPABILITY_PURPOSE.to_string();
    assert_eq!(
        harness.submit_request(&request, &telephone, START).unwrap_err().code(),
        "INVALID_CAPABILITY"
    );
}

#[test]
fn expired_credential_is_rejected() {
    let mut harness = Harness::new();
    let telephone = device(21);
    let mut request = harness.request("dev-1", &telephone, "cap-1", START);
    request.credential.expires_at = START - 1;
    let signature: Signature = harness
        .supervisor
        .sign(&request.credential.signing_bytes().unwrap());
    request.credential.signature = hex_encode(&signature.to_bytes());
    assert_eq!(
        harness.submit_request(&request, &telephone, START).unwrap_err().code(),
        "INVALID_CREDENTIAL"
    );
}

#[test]
fn malformed_and_oversized_inputs_are_rejected_with_bounded_errors() {
    let mut harness = Harness::new();
    let telephone = device(21);
    let request = harness.request("dev-1", &telephone, "cap-1", START);
    let hash = body_hash(&request);
    let public_key = request.credential.device_key().unwrap();
    let (_, base_auth, meta, _) = harness.sign_auth(
        "dev-1",
        &telephone,
        "POST",
        "/v1/devices",
        "",
        None,
        hash,
        ChallengePurpose::DeviceRegistration,
        START,
    );

    let mut cases: Vec<(SignedAuth, &str)> = Vec::new();
    let mut auth = base_auth.clone();
    auth.nonce = "zz".into();
    cases.push((auth, "INVALID_REQUEST"));
    let mut auth = base_auth.clone();
    auth.signature = "00".into();
    cases.push((auth, "INVALID_REQUEST"));
    let mut auth = base_auth.clone();
    auth.device_id = "d".repeat(300);
    cases.push((auth, "INVALID_REQUEST"));
    let mut auth = base_auth.clone();
    auth.challenge_id = "unknown-challenge".into();
    cases.push((auth, "INVALID_CHALLENGE"));
    for (auth, expected) in cases {
        let error = harness
            .service
            .verify_device_signature(
                &meta,
                &hash,
                &auth,
                &public_key,
                ChallengePurpose::DeviceRegistration,
                START,
            )
            .expect_err("malformed auth rejected");
        assert_eq!(error.code(), expected);
    }

    // 超长字段在请求形状校验处有界拒绝。
    let mut oversized = request.clone();
    oversized.credential.endpoint_id = "e".repeat(300);
    assert_eq!(
        oversized.validate_shape().unwrap_err().code(),
        "INVALID_REQUEST"
    );
    let mut oversized = request.clone();
    oversized.account_id = "a".repeat(200);
    assert_eq!(
        oversized.validate_shape().unwrap_err().code(),
        "INVALID_REQUEST"
    );
    let mut oversized = request;
    oversized.credential.device_public_key = "00".repeat(64);
    assert_eq!(
        oversized.validate_shape().unwrap_err().code(),
        "INVALID_REQUEST"
    );

    // 未消费挑战容量：超过策略上限即拒绝，不无限分配。
    let config = NodeServiceConfig {
        node_id: "node-test-2".into(),
        signing_seed: [9_u8; 32],
        policy: NodeAuthPolicy {
            max_pending_challenges: 3,
            ..NodeAuthPolicy::default()
        },
    };
    let mut bounded = NodeService::open_in_memory(config).expect("node opens");
    for _ in 0..3 {
        bounded
            .issue_auth_challenge(ChallengePurpose::RequestAuth, START)
            .expect("within capacity");
    }
    assert_eq!(
        bounded
            .issue_auth_challenge(ChallengePurpose::RequestAuth, START)
            .unwrap_err()
            .code(),
        "CHALLENGE_CAPACITY_EXCEEDED"
    );
}

#[test]
fn registered_device_request_auth_returns_actor_and_rejects_reuse() {
    let mut harness = Harness::new();
    let telephone = device(21);
    harness
        .register("dev-1", &telephone, "cap-1", START)
        .expect("registration");
    let meta = RequestMeta::canonical("POST", "/v1/auth/challenge", "").unwrap();
    let hash = sha256(b"body");
    let (_, auth, _, signed_hash) = harness.sign_auth(
        "dev-1",
        &telephone,
        "POST",
        "/v1/auth/challenge",
        "",
        None,
        hash,
        ChallengePurpose::RequestAuth,
        START,
    );
    let actor: AuthenticatedDevice = harness
        .service
        .authenticate_request(&meta, &signed_hash, &auth, ChallengePurpose::RequestAuth, START)
        .expect("registered device authenticates");
    assert_eq!(actor.account_id, "acct-test-1");
    assert_eq!(actor.device_id, "dev-1");
    assert_eq!(actor.credential_version, 1);
    let error = harness
        .service
        .authenticate_request(&meta, &signed_hash, &auth, ChallengePurpose::RequestAuth, START)
        .expect_err("replay rejected");
    assert_eq!(error.code(), "AUTH_REPLAY");

    // 未登记设备不能走已登记鉴权路径。
    let (_, unknown_auth, _, unknown_hash) = harness.sign_auth(
        "dev-unknown",
        &device(55),
        "POST",
        "/v1/auth/challenge",
        "",
        None,
        hash,
        ChallengePurpose::RequestAuth,
        START,
    );
    let error = harness
        .service
        .authenticate_request(&meta, &unknown_hash, &unknown_auth, ChallengePurpose::RequestAuth, START)
        .expect_err("unknown device rejected");
    assert_eq!(error.code(), "INVALID_CREDENTIAL");
}

#[test]
fn node_identity_is_pinned_across_restart() {
    let directory = std::env::temp_dir().join(format!("chat-node-identity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("temp dir");
    let path = directory.join("node.db");
    let config = NodeServiceConfig {
        node_id: "node-pinned".into(),
        signing_seed: [3_u8; 32],
        policy: NodeAuthPolicy::default(),
    };
    {
        let service = NodeService::open(&path, config.clone()).expect("first open");
        assert_eq!(service.node_id(), "node-pinned");
    }
    {
        let service = NodeService::open(&path, config).expect("reopen with same identity");
        assert_eq!(
            service.node_public_key(),
            SigningKey::from_bytes(&[3_u8; 32]).verifying_key().to_bytes()
        );
    }
    let mismatched = NodeServiceConfig {
        node_id: "node-pinned".into(),
        signing_seed: [4_u8; 32],
        policy: NodeAuthPolicy::default(),
    };
    assert_eq!(
        NodeService::open(&path, mismatched).unwrap_err().code(),
        "NODE_IDENTITY_MISMATCH"
    );
    let _ = std::fs::remove_dir_all(&directory);
}

/// 并发签发挑战时，成功数量不会突破全局容量上限。
#[test]
fn concurrent_challenge_issuance_stays_within_capacity() {
    use std::sync::{Arc, Mutex};

    let config = NodeServiceConfig {
        node_id: "node-concurrent-1".into(),
        signing_seed: [5_u8; 32],
        policy: NodeAuthPolicy {
            max_pending_challenges: 8,
            ..NodeAuthPolicy::default()
        },
    };
    let service = Arc::new(Mutex::new(
        NodeService::open_in_memory(config).expect("node opens"),
    ));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let service = Arc::clone(&service);
        handles.push(std::thread::spawn(move || {
            let mut accepted = 0_usize;
            for _ in 0..8 {
                let mut guard = service.lock().expect("service lock");
                if guard
                    .issue_auth_challenge(ChallengePurpose::RequestAuth, START)
                    .is_ok()
                {
                    accepted += 1;
                }
            }
            accepted
        }));
    }
    let accepted: usize = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread joins"))
        .sum();
    assert_eq!(accepted, 8, "capacity is a hard bound under concurrency");
}

/// 同一个挑战被并发消费时，最多只有一个请求成功。
#[test]
fn concurrent_reuse_of_one_challenge_succeeds_at_most_once() {
    use std::sync::{Arc, Mutex};

    let config = NodeServiceConfig {
        node_id: "node-concurrent-2".into(),
        signing_seed: [6_u8; 32],
        policy: NodeAuthPolicy::default(),
    };
    let service = Arc::new(Mutex::new(
        NodeService::open_in_memory(config).expect("node opens"),
    ));
    let device = device(71);
    let challenge = {
        let mut guard = service.lock().expect("service lock");
        guard
            .issue_auth_challenge(ChallengePurpose::RequestAuth, START)
            .expect("challenge")
    };
    let nonce = hex_decode::<32>("nonce", &challenge.nonce).expect("nonce hex");
    let meta = RequestMeta::canonical("POST", "/v1/auth/challenge", "").unwrap();
    let body_hash = sha256(b"concurrent body");
    let node_id = "node-concurrent-2".to_owned();
    let signed = request_signing_bytes(
        "dev-71",
        &meta,
        &body_hash,
        &nonce,
        &node_id,
        ChallengePurpose::RequestAuth,
    );
    let signature: Signature = device.sign(&signed);
    let auth = SignedAuth {
        challenge_id: challenge.challenge_id.clone(),
        device_id: "dev-71".into(),
        nonce: challenge.nonce.clone(),
        signature: hex_encode(&signature.to_bytes()),
    };
    let device_public_key = device.verifying_key().to_bytes();
    let mut handles = Vec::new();
    for _ in 0..4 {
        let service = Arc::clone(&service);
        let auth = auth.clone();
        let meta = meta.clone();
        handles.push(std::thread::spawn(move || {
            let mut guard = service.lock().expect("service lock");
            guard
                .verify_device_signature(
                    &meta,
                    &body_hash,
                    &auth,
                    &device_public_key,
                    ChallengePurpose::RequestAuth,
                    START,
                )
                .is_ok()
        }));
    }
    let successes = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread joins"))
        .filter(|succeeded| *succeeded)
        .count();
    // 未登记设备只用于“证明私钥”的挑战消费，成功证明恰好一次。
    assert_eq!(successes, 1, "a challenge can be consumed exactly once");
}

/// 已登记设备不能替另一个账户登记设备（跨账户请求证明拒绝）。
#[test]
fn registered_device_cannot_register_for_another_account() {
    let mut harness = Harness::new();
    let telephone = device(21);
    harness
        .register("dev-1", &telephone, "cap-1", START)
        .expect("first registration");

    let other_supervisor = device(60);
    let other_account = "acct-other".to_owned();
    let mut credential = DeviceCredential {
        account_id: other_account.clone(),
        device_id: "dev-1".into(),
        device_public_key: hex_encode(&telephone.verifying_key().to_bytes()),
        endpoint_id: "endpoint-other".into(),
        credential_version: 1,
        not_before: START - 10,
        expires_at: START + 3_600,
        supervisor_public_key: hex_encode(&other_supervisor.verifying_key().to_bytes()),
        signature: String::new(),
    };
    let signature: Signature = other_supervisor.sign(&credential.signing_bytes().unwrap());
    credential.signature = hex_encode(&signature.to_bytes());
    let mut capability = RegistrationCapability {
        capability_id: "cap-other".into(),
        account_id: other_account.clone(),
        device_id: "dev-1".into(),
        purpose: REGISTRATION_CAPABILITY_PURPOSE.into(),
        issued_at: START - 10,
        expires_at: START + 600,
        revocation_version: 1,
        supervisor_public_key: hex_encode(&other_supervisor.verifying_key().to_bytes()),
        signature: String::new(),
    };
    let signature: Signature = other_supervisor.sign(&capability.signing_bytes().unwrap());
    capability.signature = hex_encode(&signature.to_bytes());
    let request = RegisterDeviceRequest {
        account_id: other_account,
        credential,
        capability,
    };
    let hash = body_hash(&request);
    let (_, auth, meta, _) = harness.sign_auth(
        "dev-1",
        &telephone,
        "POST",
        "/v1/devices",
        "",
        None,
        hash,
        ChallengePurpose::DeviceRegistration,
        START,
    );
    let proof = harness
        .service
        .verify_device_signature(
            &meta,
            &hash,
            &auth,
            &telephone.verifying_key().to_bytes(),
            ChallengePurpose::DeviceRegistration,
            START,
        )
        .expect("registered device proves possession");
    let error = harness
        .service
        .register_device(&request, &proof, START)
        .expect_err("cross-account registration rejected");
    assert_eq!(error.code(), "PERMISSION_DENIED");
}