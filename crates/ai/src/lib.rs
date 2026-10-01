//! Embedded Codex App Server runtime and stdio JSON-RPC transport.
//!
//! This crate is intentionally GPUI- and Tokio-free: it is plain
//! thread/channel Rust so it can be exercised and tested without pulling in
//! the UI toolchain. See `docs/adr/0032-embedded-codex-app-server-ai-foundation.md`
//! for the design this implements.

mod atomic_file;
mod connect;
mod credential_journal;
mod owner_lock;
mod paths;
mod protocol;
mod provider;
mod rpc;
mod runtime;
mod secrets;
mod settings;
mod settings_lock;

pub use connect::{
    ConfiguredRuntime, ConnectError, ExpectedCredentialState,
    build_runtime_config_for_active_connection, call_with_generation_check,
    delete_custom_credential, recover_at_startup, update_custom_credential,
    with_generation_checked_lock,
};
pub use credential_journal::{
    CredentialJournal, CredentialOperation, JournalOperationKind, JournalOperationState,
    RecoveryOutcome, recover as recover_credential_journal,
};
pub use owner_lock::{OwnerLock, OwnerLockGuard};
pub use paths::AiPaths;
pub use protocol::{ErrorObject, IncomingMessage, ParseError, RequestId};
pub use provider::{
    CUSTOM_PROVIDER_ENV_KEY, CUSTOM_PROVIDER_ID, CustomProviderConfigError, CustomProviderMaterial,
    ShellEnvironmentPolicyFormat, WriteCodexConfigError, build_custom_provider_material,
    generate_custom_provider_toml, validate_base_url, write_codex_config,
};
pub use rpc::{RejectAllServerRequests, RpcError, RpcEvent, ServerRequestHandler};
pub use runtime::{
    AiRuntime, RuntimeConfig, RuntimeError, RuntimeEvent, RuntimeEventKind, RuntimeState,
    RuntimeStatus,
};
pub use secrets::{
    CredentialRef, CredentialStore, CredentialStoreError, FakeCredentialStore, OsCredentialStore,
    UnavailableCredentialStore,
};
pub use settings::{
    AI_SETTINGS_SCHEMA_VERSION, ActiveConnection, AiSettings, AiSettingsStore,
    ChatGptConnectionSettings, CustomConnectionSettings, SaveError,
};
pub use settings_lock::{AiSettingsExclusiveGuard, AiSettingsLock, AiSettingsSharedGuard};
