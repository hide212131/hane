//! Embedded Codex App Server runtime and stdio JSON-RPC transport.
//!
//! This crate is intentionally GPUI- and Tokio-free: it is plain
//! thread/channel Rust so it can be exercised and tested without pulling in
//! the UI toolchain. See `docs/adr/0032-embedded-codex-app-server-ai-foundation.md`
//! for the design this implements.

mod atomic_file;
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

pub use credential_journal::{
    recover as recover_credential_journal, CredentialJournal, CredentialOperation, JournalOperationKind,
    JournalOperationState, RecoveryOutcome,
};
pub use owner_lock::{OwnerLock, OwnerLockGuard};
pub use paths::AiPaths;
pub use protocol::{ErrorObject, IncomingMessage, ParseError, RequestId};
pub use provider::{
    build_custom_provider_material, generate_custom_provider_toml, validate_base_url, write_codex_config,
    CustomProviderConfigError, CustomProviderMaterial, ShellEnvironmentPolicyFormat, CUSTOM_PROVIDER_ENV_KEY,
    CUSTOM_PROVIDER_ID,
};
pub use rpc::{RejectAllServerRequests, RpcError, RpcEvent, ServerRequestHandler};
pub use runtime::{
    AiRuntime, RuntimeConfig, RuntimeError, RuntimeEvent, RuntimeEventKind, RuntimeState, RuntimeStatus,
};
pub use secrets::{
    CredentialRef, CredentialStore, CredentialStoreError, FakeCredentialStore, OsCredentialStore,
    UnavailableCredentialStore,
};
pub use settings::{
    ActiveConnection, AiSettings, AiSettingsStore, ChatGptConnectionSettings, CustomConnectionSettings, SaveError,
    AI_SETTINGS_SCHEMA_VERSION,
};
pub use settings_lock::{AiSettingsExclusiveGuard, AiSettingsLock, AiSettingsSharedGuard};
