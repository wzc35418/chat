# 函数总索引

共 197 个函数契约，状态均为待实现。签名用于设计评审，Rust 章节的类型与 trait 尚未全部转成源码，不承诺逐字编译。各函数有独立前置条件、步骤、事务/并发、失败与验收。

|编号|函数|目标文件|阶段|
|---|---|---|---|
|PROTOCOL-001|[PROTOCOL-001 parse_id](protocol.md#protocol-001)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-002|[PROTOCOL-002 encode_canonical](protocol.md#protocol-002)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-003|[PROTOCOL-003 decode_bounded](protocol.md#protocol-003)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-004|[PROTOCOL-004 candidate_signing_bytes](protocol.md#protocol-004)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-005|[PROTOCOL-005 verify_candidate](protocol.md#protocol-005)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-006|[PROTOCOL-006 receipt_signing_bytes](protocol.md#protocol-006)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-007|[PROTOCOL-007 verify_log_entry](protocol.md#protocol-007)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-008|[PROTOCOL-008 encode_application_event](protocol.md#protocol-008)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-009|[PROTOCOL-009 verify_inner_event](protocol.md#protocol-009)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-010|[PROTOCOL-010 validate_extension](protocol.md#protocol-010)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-011|[PROTOCOL-011 negotiate_capabilities](protocol.md#protocol-011)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-012|[PROTOCOL-012 encode_page_cursor](protocol.md#protocol-012)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-013|[PROTOCOL-013 decode_page_cursor](protocol.md#protocol-013)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|PROTOCOL-014|[PROTOCOL-014 map_error](protocol.md#protocol-014)|[crates/protocol/src/wire.rs](../../crates/protocol/src/wire.rs)|P0 → P1|
|DOMAIN-001|[DOMAIN-001 validate_send_permission](domain.md#domain-001)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-002|[DOMAIN-002 authorize_group_operation](domain.md#domain-002)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-003|[DOMAIN-003 validate_capacity](domain.md#domain-003)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-004|[DOMAIN-004 apply_policy_operation](domain.md#domain-004)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-005|[DOMAIN-005 verify_control_result](domain.md#domain-005)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-006|[DOMAIN-006 validate_owner_exit](domain.md#domain-006)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-007|[DOMAIN-007 validate_ownership_transfer](domain.md#domain-007)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-008|[DOMAIN-008 transition_send_state](domain.md#domain-008)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-009|[DOMAIN-009 aggregate_delivery](domain.md#domain-009)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-010|[DOMAIN-010 classify_retry](domain.md#domain-010)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-011|[DOMAIN-011 validate_reply_target](domain.md#domain-011)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|DOMAIN-012|[DOMAIN-012 enforce_join_visibility](domain.md#domain-012)|[crates/domain/src/rules.rs](../../crates/domain/src/rules.rs)|P1 → P3|
|IDENTITY-001|[IDENTITY-001 create_account](identity.md#identity-001)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-002|[IDENTITY-002 current_account](identity.md#identity-002)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-003|[IDENTITY-003 verify_credential](identity.md#identity-003)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-004|[IDENTITY-004 create_contact_invite](identity.md#identity-004)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-005|[IDENTITY-005 accept_contact_invite](identity.md#identity-005)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-006|[IDENTITY-006 confirm_contact](identity.md#identity-006)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-007|[IDENTITY-007 verification_fingerprint](identity.md#identity-007)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-008|[IDENTITY-008 begin_device_link](identity.md#identity-008)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-009|[IDENTITY-009 authorize_device](identity.md#identity-009)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-010|[IDENTITY-010 complete_device_link](identity.md#identity-010)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-011|[IDENTITY-011 list_devices](identity.md#identity-011)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-012|[IDENTITY-012 revoke_device](identity.md#identity-012)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-013|[IDENTITY-013 export_identity_recovery](identity.md#identity-013)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|IDENTITY-014|[IDENTITY-014 import_identity_recovery](identity.md#identity-014)|[crates/identity/src/service.rs](../../crates/identity/src/service.rs)|P1 → P2|
|CRYPTO-001|[CRYPTO-001 create_crypto_group](crypto.md#crypto-001)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-002|[CRYPTO-002 generate_key_package](crypto.md#crypto-002)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-003|[CRYPTO-003 validate_key_package](crypto.md#crypto-003)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-004|[CRYPTO-004 prepare_application](crypto.md#crypto-004)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-005|[CRYPTO-005 prepare_control](crypto.md#crypto-005)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-006|[CRYPTO-006 process_verified_entry](crypto.md#crypto-006)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-007|[CRYPTO-007 merge_accepted_pending](crypto.md#crypto-007)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-008|[CRYPTO-008 discard_rejected_pending](crypto.md#crypto-008)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-009|[CRYPTO-009 join_from_welcome](crypto.md#crypto-009)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-010|[CRYPTO-010 publish_committed_state](crypto.md#crypto-010)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-011|[CRYPTO-011 encrypt_attachment_chunk](crypto.md#crypto-011)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|CRYPTO-012|[CRYPTO-012 decrypt_attachment_chunk](crypto.md#crypto-012)|[crates/crypto/src/engine.rs](../../crates/crypto/src/engine.rs)|P0 → P3|
|STORAGE-001|[STORAGE-001 open_store](storage.md#storage-001)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-002|[STORAGE-002 migrate_schema](storage.md#storage-002)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-003|[STORAGE-003 begin_transaction](storage.md#storage-003)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-004|[STORAGE-004 crypto_provider_read](storage.md#storage-004)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-005|[STORAGE-005 crypto_provider_write](storage.md#storage-005)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-006|[STORAGE-006 persist_send_intent](storage.md#storage-006)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-007|[STORAGE-007 persist_prepared_attempt](storage.md#storage-007)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-008|[STORAGE-008 apply_verified_entry](storage.md#storage-008)|[crates/storage/src/formal/entry.rs](../../crates/storage/src/formal/entry.rs)|P0 → P2|
|STORAGE-009|[STORAGE-009 commit_transaction](storage.md#storage-009)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-010|[STORAGE-010 rollback_transaction](storage.md#storage-010)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-011|[STORAGE-011 recover_incomplete_writes](storage.md#storage-011)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-012|[STORAGE-012 query_message_page](storage.md#storage-012)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-013|[STORAGE-013 query_conversation_page](storage.md#storage-013)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-014|[STORAGE-014 claim_outbox_batch](storage.md#storage-014)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|STORAGE-015|[STORAGE-015 mark_outbox_result](storage.md#storage-015)|[crates/storage/src/store.rs](../../crates/storage/src/store.rs)|P0 → P2|
|TRANSPORT-001|[TRANSPORT-001 start_endpoint](transport.md#transport-001)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-002|[TRANSPORT-002 verify_discovery_record](transport.md#transport-002)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-003|[TRANSPORT-003 connect_peer](transport.md#transport-003)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-004|[TRANSPORT-004 send_peer_frame](transport.md#transport-004)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-005|[TRANSPORT-005 read_peer_frame](transport.md#transport-005)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-006|[TRANSPORT-006 append_candidate](transport.md#transport-006)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-007|[TRANSPORT-007 fetch_group_entries](transport.md#transport-007)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-008|[TRANSPORT-008 put_mailbox](transport.md#transport-008)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-009|[TRANSPORT-009 pull_mailbox](transport.md#transport-009)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-010|[TRANSPORT-010 ack_mailbox](transport.md#transport-010)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-011|[TRANSPORT-011 subscribe_node_hints](transport.md#transport-011)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|TRANSPORT-012|[TRANSPORT-012 shutdown_transport](transport.md#transport-012)|[crates/transport/src/network.rs](../../crates/transport/src/network.rs)|P0 → P1|
|SYNC-001|[SYNC-001 enqueue_job](sync.md#sync-001)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-002|[SYNC-002 run_outbox_once](sync.md#sync-002)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-003|[SYNC-003 prepare_draft_job](sync.md#sync-003)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-004|[SYNC-004 submit_attempt_job](sync.md#sync-004)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-005|[SYNC-005 rebuild_stale_attempt](sync.md#sync-005)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-006|[SYNC-006 sync_now](sync.md#sync-006)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-007|[SYNC-007 consume_mailbox_page](sync.md#sync-007)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-008|[SYNC-008 catch_up_group](sync.md#sync-008)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-009|[SYNC-009 apply_log_page](sync.md#sync-009)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-010|[SYNC-010 process_delivery_receipt](sync.md#sync-010)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-011|[SYNC-011 schedule_retry](sync.md#sync-011)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-012|[SYNC-012 set_lifecycle](sync.md#sync-012)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-013|[SYNC-013 set_connectivity](sync.md#sync-013)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|SYNC-014|[SYNC-014 configure_node](sync.md#sync-014)|[crates/sync/src/engine.rs](../../crates/sync/src/engine.rs)|P1 → P3|
|CORE-001|[CORE-001 start_core](core.md#core-001)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-002|[CORE-002 close_core](core.md#core-002)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-003|[CORE-003 observe_events](core.md#core-003)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-004|[CORE-004 open_direct](core.md#core-004)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-005|[CORE-005 list_conversations](core.md#core-005)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-006|[CORE-006 list_messages](core.md#core-006)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-007|[CORE-007 send_message](core.md#core-007)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-008|[CORE-008 retry_draft](core.md#core-008)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-009|[CORE-009 cancel_queued](core.md#core-009)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-010|[CORE-010 get_draft_status](core.md#core-010)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-011|[CORE-011 get_delivery](core.md#core-011)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|CORE-012|[CORE-012 handle_peer_pretransfer](core.md#core-012)|[crates/core/src/service.rs](../../crates/core/src/service.rs)|P1 → P2|
|GROUPS-001|[GROUPS-001 create_group](groups.md#groups-001)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-002|[GROUPS-002 group_snapshot](groups.md#groups-002)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-003|[GROUPS-003 propose_group_operation](groups.md#groups-003)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-004|[GROUPS-004 accept_group_invite](groups.md#groups-004)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-005|[GROUPS-005 decline_group_invite](groups.md#groups-005)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-006|[GROUPS-006 prepare_group_operation](groups.md#groups-006)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-007|[GROUPS-007 operation_status](groups.md#groups-007)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-008|[GROUPS-008 remove_account_devices](groups.md#groups-008)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-009|[GROUPS-009 rename_group](groups.md#groups-009)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-010|[GROUPS-010 transfer_ownership](groups.md#groups-010)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-011|[GROUPS-011 leave_group](groups.md#groups-011)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|GROUPS-012|[GROUPS-012 dissolve_group](groups.md#groups-012)|[crates/core/src/groups.rs](../../crates/core/src/groups.rs)|P3|
|ATTACHMENTS-001|[ATTACHMENTS-001 import_file](attachments.md#attachments-001)|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|P2|
|ATTACHMENTS-002|[ATTACHMENTS-002 attachment_status](attachments.md#attachments-002)|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|P2|
|ATTACHMENTS-003|[ATTACHMENTS-003 upload_attachment](attachments.md#attachments-003)|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|P2|
|ATTACHMENTS-004|[ATTACHMENTS-004 build_attachment_descriptor](attachments.md#attachments-004)|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|P2|
|ATTACHMENTS-005|[ATTACHMENTS-005 download_attachment](attachments.md#attachments-005)|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|P2|
|ATTACHMENTS-006|[ATTACHMENTS-006 export_attachment](attachments.md#attachments-006)|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|P2|
|ATTACHMENTS-007|[ATTACHMENTS-007 cancel_transfer](attachments.md#attachments-007)|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|P2|
|ATTACHMENTS-008|[ATTACHMENTS-008 collect_unreferenced_attachments](attachments.md#attachments-008)|[crates/core/src/attachments.rs](../../crates/core/src/attachments.rs)|P2|
|NODE-001|[NODE-001 register_device](node.md#node-001)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-002|[NODE-002 publish_discovery](node.md#node-002)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-003|[NODE-003 publish_key_packages](node.md#node-003)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-004|[NODE-004 claim_key_package](node.md#node-004)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-005|[NODE-005 register_group](node.md#node-005)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-006|[NODE-006 append_group_entry](node.md#node-006)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-007|[NODE-007 fetch_group_log](node.md#node-007)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-008|[NODE-008 get_group_head](node.md#node-008)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-009|[NODE-009 store_mailbox_item](node.md#node-009)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-010|[NODE-010 read_mailbox_page](node.md#node-010)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-011|[NODE-011 ack_mailbox_items](node.md#node-011)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-012|[NODE-012 begin_blob_upload](node.md#node-012)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-013|[NODE-013 put_blob_chunk](node.md#node-013)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-014|[NODE-014 complete_blob_upload](node.md#node-014)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-015|[NODE-015 read_blob_chunk](node.md#node-015)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|NODE-016|[NODE-016 expire_node_data](node.md#node-016)|[crates/node-services/src/services.rs](../../crates/node-services/src/services.rs)|P1 → P5|
|API-001|[API-001 load_node_config](api.md#api-001)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-002|[API-002 issue_auth_challenge](api.md#api-002)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-003|[API-003 authenticate_request](api.md#api-003)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-004|[API-004 validate_capability](api.md#api-004)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-005|[API-005 enforce_rate_limit](api.md#api-005)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-006|[API-006 handle_append_http](api.md#api-006)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-007|[API-007 handle_mailbox_http](api.md#api-007)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-008|[API-008 handle_blob_http](api.md#api-008)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-009|[API-009 upgrade_event_socket](api.md#api-009)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|API-010|[API-010 run_node](api.md#api-010)|[crates/node-api/src/handlers.rs](../../crates/node-api/src/handlers.rs)|P1 → P5|
|BRIDGE-001|[BRIDGE-001 ffi_create_core](bridge.md#bridge-001)|[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)|P0 → P2|
|BRIDGE-002|[BRIDGE-002 ffi_dispatch](bridge.md#bridge-002)|[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)|P0 → P2|
|BRIDGE-003|[BRIDGE-003 ffi_subscribe](bridge.md#bridge-003)|[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)|P0 → P2|
|BRIDGE-004|[BRIDGE-004 release_ffi_subscription](bridge.md#bridge-004)|[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)|P0 → P2|
|BRIDGE-005|[BRIDGE-005 stop_event_bridge](bridge.md#bridge-005)|[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)|P0 → P2|
|BRIDGE-006|[BRIDGE-006 bridge_event_flow](bridge.md#bridge-006)|[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)|P0 → P2|
|BRIDGE-007|[BRIDGE-007 bridge_suspend_call](bridge.md#bridge-007)|[crates/ffi/src/bridge.rs](../../crates/ffi/src/bridge.rs)|P0 → P2|
|PLATFORM-001|[PLATFORM-001 wrap_storage_key](platform.md#platform-001)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-002|[PLATFORM-002 unwrap_storage_key](platform.md#platform-002)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-003|[PLATFORM-003 delete_key_alias](platform.md#platform-003)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-004|[PLATFORM-004 choose_import_file](platform.md#platform-004)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-005|[PLATFORM-005 choose_export_file](platform.md#platform-005)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-006|[PLATFORM-006 open_file_token](platform.md#platform-006)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-007|[PLATFORM-007 show_notification](platform.md#platform-007)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-008|[PLATFORM-008 cancel_notification](platform.md#platform-008)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-009|[PLATFORM-009 handle_platform_lifecycle](platform.md#platform-009)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PLATFORM-010|[PLATFORM-010 desktop_exit](platform.md#platform-010)|[shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt](../../shared/platform-api/src/commonMain/kotlin/chat/platform/Services.kt)|P0 → P2|
|PRESENTATION-001|[PRESENTATION-001 bootstrap_application](presentation.md#presentation-001)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-002|[PRESENTATION-002 load_conversation_list](presentation.md#presentation-002)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-003|[PRESENTATION-003 load_message_page](presentation.md#presentation-003)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-004|[PRESENTATION-004 submit_composer](presentation.md#presentation-004)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-005|[PRESENTATION-005 retry_from_ui](presentation.md#presentation-005)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-006|[PRESENTATION-006 cancel_from_ui](presentation.md#presentation-006)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-007|[PRESENTATION-007 submit_group_action](presentation.md#presentation-007)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-008|[PRESENTATION-008 handle_core_event](presentation.md#presentation-008)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-009|[PRESENTATION-009 render_message_content](presentation.md#presentation-009)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-010|[PRESENTATION-010 present_error](presentation.md#presentation-010)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PRESENTATION-011|[PRESENTATION-011 build_adaptive_layout](presentation.md#presentation-011)|[shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt](../../shared/presentation/src/commonMain/kotlin/chat/presentation/UseCases.kt)|P2 → P4|
|PLUGINS-001|[PLUGINS-001 inspect_plugin_package](plugins.md#plugins-001)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-002|[PLUGINS-002 install_trusted_plugin](plugins.md#plugins-002)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-003|[PLUGINS-003 discover_plugins](plugins.md#plugins-003)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-004|[PLUGINS-004 start_plugin](plugins.md#plugins-004)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-005|[PLUGINS-005 register_plugin_command](plugins.md#plugins-005)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-006|[PLUGINS-006 register_message_action](plugins.md#plugins-006)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-007|[PLUGINS-007 selected_content](plugins.md#plugins-007)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-008|[PLUGINS-008 propose_plugin_send](plugins.md#plugins-008)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-009|[PLUGINS-009 render_plugin_settings](plugins.md#plugins-009)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-010|[PLUGINS-010 stop_plugin](plugins.md#plugins-010)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-011|[PLUGINS-011 disable_plugin](plugins.md#plugins-011)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-012|[PLUGINS-012 remove_plugin](plugins.md#plugins-012)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|PLUGINS-013|[PLUGINS-013 load_theme_config](plugins.md#plugins-013)|[desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt](../../desktop/plugin-host/src/main/kotlin/chat/plugin/PluginHost.kt)|P4|
|GROUP-ACTIONS-001|[GROUP-ACTIONS-001 invite_group_member](group-actions.md#group-actions-001)|[crates/core/src/group_actions.rs](../../crates/core/src/group_actions.rs)|P3|
|GROUP-ACTIONS-002|[GROUP-ACTIONS-002 set_group_role](group-actions.md#group-actions-002)|[crates/core/src/group_actions.rs](../../crates/core/src/group_actions.rs)|P3|
|GROUP-ACTIONS-003|[GROUP-ACTIONS-003 set_group_muted](group-actions.md#group-actions-003)|[crates/core/src/group_actions.rs](../../crates/core/src/group_actions.rs)|P3|
|GROUP-ACTIONS-004|[GROUP-ACTIONS-004 sync_account_device_change](group-actions.md#group-actions-004)|[crates/core/src/group_actions.rs](../../crates/core/src/group_actions.rs)|P3|
|GROUP-ACTIONS-005|[GROUP-ACTIONS-005 rejoin_stale_device](group-actions.md#group-actions-005)|[crates/core/src/group_actions.rs](../../crates/core/src/group_actions.rs)|P3|
