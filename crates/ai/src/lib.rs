//! Embedded Codex App Server runtime and stdio JSON-RPC transport.
//!
//! This crate is intentionally GPUI- and Tokio-free: it is plain
//! thread/channel Rust so it can be exercised and tested without pulling in
//! the UI toolchain. See `docs/adr/0032-embedded-codex-app-server-ai-foundation.md`
//! for the design this implements.

mod account;
mod atomic_file;
mod connect;
mod credential_journal;
mod models;
mod owner_lock;
mod paths;
mod probe;
mod prompts;
mod protocol;
mod provider;
mod rpc;
mod runtime;
mod secrets;
mod service;
mod settings;
mod settings_lock;
mod text_transform;

pub use account::{
    AccountError, AccountState, CancelStatus, account_read_params, completed_login_matches,
    parse_account_read, parse_cancel_status, parse_chatgpt_login_start,
};
pub use connect::{
    ConfiguredRuntime, ConnectError, ExpectedCredentialState, SaveOutcome,
    build_runtime_config_for_active_connection, call_with_generation_check,
    delete_custom_credential, delete_custom_credential_detailed, recover_at_startup,
    update_custom_credential, update_custom_credential_detailed, with_generation_checked_lock,
};
pub use credential_journal::{
    CredentialJournal, CredentialOperation, JournalOperationKind, JournalOperationState,
    RecoveryOutcome, recover as recover_credential_journal,
};
pub use models::{ChatGptModel, ModelListError, fetch_chatgpt_models, saved_model_is_available};
pub use owner_lock::{OwnerLock, OwnerLockGuard};
pub use paths::AiPaths;
pub use probe::{ProbeErrorCode, ProbeResult, ProbeStatus};
pub use prompts::{
    MAX_PROMPT_BODY_BYTES, MAX_PROMPT_COUNT, MAX_PROMPT_TITLE_CHARS, USER_PROMPTS_SCHEMA_VERSION,
    UserPrompt, UserPromptDraft, UserPrompts, UserPromptsLoadError, UserPromptsSaveError,
    UserPromptsStore, initial_default_draft,
};
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
pub use service::{
    AdmissionError, AiCommand, AiService, AiServiceConfig, AiServiceHandle, AiSnapshot,
    BrowserOpenError, BrowserOpener, LoginState, ModelListState, OperationId, OwnershipState,
    PersistenceState, SafeOperationResult, ServiceBusyReason, SystemBrowserOpener,
};
pub use settings::{
    AI_SETTINGS_SCHEMA_VERSION, ActiveConnection, AiSettings, AiSettingsStore,
    ChatGptConnectionSettings, CustomConnectionSettings, SaveError,
};
pub use settings_lock::{AiSettingsExclusiveGuard, AiSettingsLock, AiSettingsSharedGuard};
pub use text_transform::{
    MAX_INSTRUCTION_BYTES, MAX_RESULT_BYTES, MAX_SELECTED_TEXT_BYTES, TEXT_TRANSFORM_TIMEOUT,
    TextTransformErrorCode, TextTransformOutcome,
    validate_request as validate_text_transform_request,
};
