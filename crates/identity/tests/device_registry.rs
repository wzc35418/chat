
//! IDENTITY-008~012 的持久化契约测试（真实加密存储，不接网络）。
//!
//! 覆盖：配对短码与 TTL 边界、授权→完成→列表→重开、有效设备上限、撤销终态与
//! 重新签发、过期拒绝、撤销后不可重联、最后一台设备保护、跨账户隔离、能力要求。

use chat_identity::{
    AccountKeys, CapabilitySet, DeviceCapability, DeviceState, IdentityError, MAX_ACTIVE_DEVICES,
    MAX_LINK_TTL_MS, MIN_LINK_TTL_MS, TrustAnchor, authorize_device, begin_device_link,
    complete_device_link, list_devices, require_active_device, revoke_device,
};
use chat_protocol::{AccountId, DeviceId};
use chat_storage::formal::{KeyProvenance, ProtectedStorageKey, Store, StoreOptions, open_store};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const NOW: u64 = 1_700_000_000_000;
const DEVICE_TTL: u64 = 60_000;
const LINK_TTL: u64 = 60_000;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    key: [u8; 32],
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "chat-identity-registry-{tag}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&dir).expect("create fixture directory");
        let path = dir.join("chat.db");
        let mut key = [0u8; 32];
        for (index, byte) in key.iter_mut().enumerate() {
            *byte = u8::try_from(index + 1).expect("byte") ^ u8::try_from(unique % 200).expect("byte");
        }
        Self { dir, path, key }
    }

    fn open(&self) -> Store {
        open_store(
            StoreOptions::new(&self.path, KeyProvenance::Raw32),
            ProtectedStorageKey::from_bytes(self.key),
        )
        .expect("open formal store")
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn account() -> AccountKeys {
    AccountKeys::from_seed(&[0x01; 32])
}

fn messaging_capabilities() -> CapabilitySet {
    CapabilitySet::try_new(&[
        DeviceCapability::SendDirect,
        DeviceCapability::SendGroup,
        DeviceCapability::Receive,
    ])
    .expect("capabilities")
}

fn expected_code(error: IdentityError, code: &str) {
    assert_eq!(error.code().as_str(), code, "unexpected error {error:?}");
}

#[test]
fn link_request_is_bounded_and_has_a_stable_verification_code() {
    let link = begin_device_link(NOW, LINK_TTL).expect("link");
    let code = link.verification_code();
    assert_eq!(code.len(), 7, "verification code must be grouped 3+3 digits");
    let parts: Vec<&str> = code.split(' ').collect();
    assert_eq!(parts.len(), 2);
    for part in &parts {
        assert_eq!(part.len(), 3);
        assert!(part.chars().all(|character| character.is_ascii_digit()));
    }
    assert_eq!(link.issued_at_ms(), NOW);
    assert_eq!(link.expires_at_ms(), NOW + LINK_TTL);
    assert!(!link.is_expired_at(NOW + LINK_TTL - 1));
    assert!(link.is_expired_at(NOW + LINK_TTL));
    // Debug 不泄漏设备私钥。
    let rendered = format!("{link:?}");
    assert!(rendered.contains("<redacted>"));
    assert!(!rendered.contains("seed"));

    assert_eq!(
        begin_device_link(NOW, 0).unwrap_err(),
        IdentityError::InvalidLinkTtl {
            max_ms: MAX_LINK_TTL_MS
        }
    );
    assert!(begin_device_link(NOW, MIN_LINK_TTL_MS - 1).is_err());
    assert!(begin_device_link(NOW, MAX_LINK_TTL_MS + 1).is_err());
    assert!(begin_device_link(NOW, MIN_LINK_TTL_MS).is_ok());
    assert!(begin_device_link(NOW, MAX_LINK_TTL_MS).is_ok());
    assert_eq!(
        begin_device_link(0, LINK_TTL).unwrap_err(),
        IdentityError::InvalidTimestamp
    );
}

#[test]
fn authorize_complete_and_list_survive_reopen() {
    let fixture = Fixture::new("roundtrip");
    let mut store = fixture.open();
    let account = account();
    let anchor = account.trust_anchor();

    let pending = begin_device_link(NOW, LINK_TTL).expect("link");
    let authorized = authorize_device(
        &mut store,
        &account,
        &pending,
        messaging_capabilities(),
        Some([0x5a; 32]),
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize");
    assert_eq!(authorized.device_id(), pending_device_id(&account, &pending));
    assert_eq!(authorized.expires_at_ms(), NOW + 1_000 + DEVICE_TTL);

    // 新设备侧：校验主管签名并保存自己的凭据。
    let verified = complete_device_link(&mut store, &pending, authorized.credential(), NOW + 1_000)
        .expect("complete link");
    assert_eq!(verified.device_id(), authorized.device_id());
    assert_eq!(verified.fingerprint(), anchor.fingerprint());

    let listed = list_devices(&store, account.account_id(), NOW + 1_000).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].device_id, authorized.device_id());
    assert_eq!(listed[0].state, DeviceState::Active);
    assert_eq!(listed[0].capabilities, messaging_capabilities());
    assert_eq!(listed[0].issued_at_ms, NOW + 1_000);
    assert_eq!(listed[0].expires_at_ms, NOW + 1_000 + DEVICE_TTL);

    // 发送前检查通过。
    let device = require_active_device(
        &store,
        &anchor,
        authorized.device_id(),
        NOW + 2_000,
        Some(DeviceCapability::SendDirect),
    )
    .expect("active device");
    assert!(device.has(DeviceCapability::SendGroup));

    drop(store);
    // 重开：凭据仍在，仍可用。
    let store = fixture.open();
    let listed = list_devices(&store, account.account_id(), NOW + 2_000).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].device_id, authorized.device_id());
    assert!(
        require_active_device(&store, &anchor, authorized.device_id(), NOW + 2_000, None).is_ok()
    );
    assert!(fixture.path().exists());
}

fn pending_device_id(account: &AccountKeys, pending: &chat_identity::PendingDeviceLink) -> DeviceId {
    chat_identity::device_id_for(account.account_id(), &pending.device_public_key())
}

#[test]
fn active_device_limit_is_two() {
    let fixture = Fixture::new("limit");
    let mut store = fixture.open();
    let account = account();
    let anchor = account.trust_anchor();

    for index in 0..MAX_ACTIVE_DEVICES {
        let pending = begin_device_link(NOW, LINK_TTL).expect("link");
        authorize_device(
            &mut store,
            &account,
            &pending,
            messaging_capabilities(),
            None,
            NOW + 1_000 + index as u64,
            DEVICE_TTL,
        )
        .expect("authorize within limit");
    }
    let pending = begin_device_link(NOW, LINK_TTL).expect("link");
    let error = authorize_device(
        &mut store,
        &account,
        &pending,
        messaging_capabilities(),
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .unwrap_err();
    assert_eq!(error, IdentityError::DeviceLimitExceeded);
    expected_code(error, "DEVICE_LIMIT_EXCEEDED");
    assert_eq!(
        list_devices(&store, account.account_id(), NOW + 1_000)
            .expect("list")
            .len(),
        MAX_ACTIVE_DEVICES
    );
    assert!(anchor.account_id() == account.account_id());
}

#[test]
fn revocation_is_terminal_and_remaining_devices_must_be_reissued() {
    let fixture = Fixture::new("revoke");
    let mut store = fixture.open();
    let account = account();
    let anchor = account.trust_anchor();

    let first = begin_device_link(NOW, LINK_TTL).expect("link");
    let first_authorized = authorize_device(
        &mut store,
        &account,
        &first,
        messaging_capabilities(),
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize first");
    let second = begin_device_link(NOW, LINK_TTL).expect("link");
    let second_authorized = authorize_device(
        &mut store,
        &account,
        &second,
        messaging_capabilities(),
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize second");

    let revocation = revoke_device(
        &mut store,
        account.account_id(),
        second_authorized.device_id(),
        NOW + 2_000,
    )
    .expect("revoke second");
    assert_eq!(revocation.revocation_generation, 1);

    // 已撤销设备：立即拒绝（DEVICE_REVOKED）。
    let error = require_active_device(
        &store,
        &anchor,
        second_authorized.device_id(),
        NOW + 3_000,
        None,
    )
    .unwrap_err();
    assert_eq!(error, IdentityError::DeviceRevoked);
    expected_code(error, "DEVICE_REVOKED");

    // 撤销推进了账户撤销版本：其余设备必须重新签发，否则按 STALE 拒绝。
    let stale = require_active_device(
        &store,
        &anchor,
        first_authorized.device_id(),
        NOW + 3_000,
        None,
    )
    .unwrap_err();
    assert_eq!(stale, IdentityError::CredentialStale);
    expected_code(stale, "INVALID_CREDENTIAL");

    // 重新签发（同一设备密钥，引用当前撤销版本）后恢复可用。
    let reissued = authorize_device(
        &mut store,
        &account,
        &first,
        messaging_capabilities(),
        None,
        NOW + 4_000,
        DEVICE_TTL,
    )
    .expect("reissue");
    assert_eq!(reissued.credential().revocation_generation, 1);
    assert!(
        require_active_device(&store, &anchor, first_authorized.device_id(), NOW + 5_000, None)
            .is_ok()
    );

    // 已撤销设备不能通过重新授权复活。
    let error = authorize_device(
        &mut store,
        &account,
        &second,
        messaging_capabilities(),
        None,
        NOW + 4_000,
        DEVICE_TTL,
    )
    .unwrap_err();
    assert_eq!(error, IdentityError::DeviceRevoked);

    // 列表仍显示两台设备，其中一台为 revoked。
    let listed = list_devices(&store, account.account_id(), NOW + 5_000).expect("list");
    assert_eq!(listed.len(), 2);
    assert!(
        listed
            .iter()
            .any(|device| device.device_id == second_authorized.device_id()
                && device.state == DeviceState::Revoked)
    );
}

#[test]
fn expired_credentials_are_rejected_and_free_a_slot() {
    let fixture = Fixture::new("expiry");
    let mut store = fixture.open();
    let account = account();
    let anchor = account.trust_anchor();

    let pending = begin_device_link(NOW, LINK_TTL).expect("link");
    let authorized = authorize_device(
        &mut store,
        &account,
        &pending,
        messaging_capabilities(),
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize");
    let expires_at = authorized.expires_at_ms();

    // 失效时刻即拒绝。
    let error =
        require_active_device(&store, &anchor, authorized.device_id(), expires_at, None).unwrap_err();
    assert_eq!(error, IdentityError::CredentialExpired);
    expected_code(error, "INVALID_CREDENTIAL");
    assert!(
        require_active_device(&store, &anchor, authorized.device_id(), expires_at - 1, None).is_ok()
    );

    // 过期设备在列表里是 Expired，且不再占用有效设备名额。
    let listed = list_devices(&store, account.account_id(), expires_at).expect("list");
    assert_eq!(listed[0].state, DeviceState::Expired);
    let fresh = begin_device_link(expires_at, LINK_TTL).expect("link");
    authorize_device(
        &mut store,
        &account,
        &fresh,
        messaging_capabilities(),
        None,
        expires_at,
        DEVICE_TTL,
    )
    .expect("expired devices must not block a new link");
}

#[test]
fn last_active_device_cannot_be_revoked_directly() {
    let fixture = Fixture::new("last-device");
    let mut store = fixture.open();
    let account = account();

    let first = begin_device_link(NOW, LINK_TTL).expect("link");
    let first_authorized = authorize_device(
        &mut store,
        &account,
        &first,
        messaging_capabilities(),
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize");
    let error = revoke_device(
        &mut store,
        account.account_id(),
        first_authorized.device_id(),
        NOW + 2_000,
    )
    .unwrap_err();
    assert_eq!(error, IdentityError::LastDeviceRequiresRecovery);
    expected_code(error, "PERMISSION_DENIED");

    let second = begin_device_link(NOW, LINK_TTL).expect("link");
    let second_authorized = authorize_device(
        &mut store,
        &account,
        &second,
        messaging_capabilities(),
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize second");
    revoke_device(
        &mut store,
        account.account_id(),
        second_authorized.device_id(),
        NOW + 2_000,
    )
    .expect("revoking a non-last device is allowed");
}

#[test]
fn link_mismatch_and_expired_links_are_rejected() {
    let fixture = Fixture::new("link");
    let mut store = fixture.open();
    let account = account();

    let pending = begin_device_link(NOW, LINK_TTL).expect("link");
    // 用另一台设备的待配对请求签发的凭据不能配给本机。
    let other = begin_device_link(NOW, LINK_TTL).expect("other link");
    let other_authorized = authorize_device(
        &mut store,
        &account,
        &other,
        messaging_capabilities(),
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize other");
    let error =
        complete_device_link(&mut store, &pending, other_authorized.credential(), NOW + 1_000)
            .unwrap_err();
    assert_eq!(error, IdentityError::LinkMismatch);

    // 过期配对请求：授权与完成都必须显式失败（不能复用旧 nonce）。
    let expired_at = NOW + LINK_TTL;
    let error = authorize_device(
        &mut store,
        &account,
        &pending,
        messaging_capabilities(),
        None,
        expired_at,
        DEVICE_TTL,
    )
    .unwrap_err();
    assert_eq!(error, IdentityError::LinkExpired);
    let error = complete_device_link(&mut store, &pending, other_authorized.credential(), expired_at)
        .unwrap_err();
    assert_eq!(error, IdentityError::LinkExpired);
}

#[test]
fn cross_account_and_unknown_devices_are_isolated() {
    let fixture = Fixture::new("isolation");
    let mut store = fixture.open();
    let account = account();
    let pending = begin_device_link(NOW, LINK_TTL).expect("link");
    let authorized = authorize_device(
        &mut store,
        &account,
        &pending,
        messaging_capabilities(),
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize");

    // 另一个账户：看不到本账户设备。
    let other_account = AccountId::from_bytes([0x99; 32]);
    assert!(
        list_devices(&store, other_account, NOW + 2_000)
            .expect("list")
            .is_empty()
    );
    let other_anchor =
        TrustAnchor::from_public_key(AccountKeys::from_seed(&[0x02; 32]).public_key())
            .expect("anchor");
    let error = require_active_device(
        &store,
        &other_anchor,
        authorized.device_id(),
        NOW + 2_000,
        None,
    )
    .unwrap_err();
    assert_eq!(error, IdentityError::DeviceNotFound);

    // 未知设备。
    let unknown = DeviceId::from_bytes([0x77; 32]);
    let error = require_active_device(&store, &account.trust_anchor(), unknown, NOW + 2_000, None)
        .unwrap_err();
    assert_eq!(error, IdentityError::DeviceNotFound);
}

#[test]
fn capability_requirements_are_enforced() {
    let fixture = Fixture::new("capability");
    let mut store = fixture.open();
    let account = account();
    let anchor = account.trust_anchor();
    let pending = begin_device_link(NOW, LINK_TTL).expect("link");
    let receive_only =
        CapabilitySet::try_new(&[DeviceCapability::Receive]).expect("capabilities");
    let authorized = authorize_device(
        &mut store,
        &account,
        &pending,
        receive_only,
        None,
        NOW + 1_000,
        DEVICE_TTL,
    )
    .expect("authorize");
    let error = require_active_device(
        &store,
        &anchor,
        authorized.device_id(),
        NOW + 2_000,
        Some(DeviceCapability::SendGroup),
    )
    .unwrap_err();
    assert_eq!(error, IdentityError::MissingCapability);
    expected_code(error, "REQUIRED_CAPABILITY_MISSING");
    assert!(
        require_active_device(
            &store,
            &anchor,
            authorized.device_id(),
            NOW + 2_000,
            Some(DeviceCapability::Receive)
        )
        .is_ok()
    );
}
