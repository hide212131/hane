//! Custom Provider Codex configuration generation.
//!
//! Per `docs/adr/0032-embedded-codex-app-server-ai-foundation.md` sections
//! 7 and 10: the Custom Provider connection is a Responses-API-compatible
//! (`wire_api = "responses"`) endpoint, configured through a dedicated
//! `model_providers.hane_custom` table. The API key itself never appears in
//! the generated TOML: it is referenced only by `env_key`, and the actual
//! secret is injected solely into the Custom connection's own App Server
//! child process environment (`extra_env`), never written to disk, never
//! logged, never placed in a Debug-formatted field.
//!
//! The Base URL is validated before it is ever embedded in generated config:
//! HTTPS is required except for an explicit local development host
//! (`localhost` / `127.0.0.1` / `::1` over plain HTTP), and a URL carrying
//! embedded credentials (`user:pass@host` / bare `user@host`) is rejected
//! outright rather than silently accepted.

use std::io;
use std::path::Path;

use crate::atomic_file::{AtomicWriteError, atomic_write_bytes};

/// The `model_providers` table key and `model_provider` selector Hane's
/// generated config always uses for the Custom Provider connection.
pub const CUSTOM_PROVIDER_ID: &str = "hane_custom";

/// The environment variable name the generated config's `env_key` points at.
/// Only ever set in the Custom connection's own App Server child process
/// environment (see `CustomProviderMaterial::extra_env`); never inherited
/// from Hane's own process environment and never written into the generated
/// TOML itself.
pub const CUSTOM_PROVIDER_ENV_KEY: &str = "HANE_AI_PROVIDER_KEY";

/// Codex's default project-instruction budget is nonzero, which lets the
/// App Server load `AGENTS.md` from its runtime cwd and add that file to a
/// turn. Hane's AI runtime must never inherit instructions from its probe
/// workspace or another nearby project.
pub const PROJECT_DOC_MAX_BYTES: usize = 0;

/// The App Server otherwise searches parent directories for project config.
/// An empty marker list confines project discovery to Hane's fresh runtime
/// workspace instead of reading a nearby repository's `.codex/config.toml`.
const PROJECT_ROOT_MARKERS: &str = "[]";

/// Codex 0.157.1 separately discovers skills from the OS home directory
/// (`~/.agents/skills`), not just from `CODEX_HOME`. Disable host skill
/// snapshots for Hane's standalone runtime; the pinned App Server supports
/// this feature when no registered extension requires host discovery.
const SKIP_HOST_SKILL_DISCOVERY: bool = true;

/// Minimal generated config for the ChatGPT-owned `CODEX_HOME`. It shares
/// the same external-context isolation settings as the Custom Provider
/// config, while leaving account authentication to the App Server API.
pub fn generate_chatgpt_config_toml() -> String {
    format!(
        "project_doc_max_bytes = {PROJECT_DOC_MAX_BYTES}\n\
         project_root_markers = {PROJECT_ROOT_MARKERS}\n\
         \n\
         [features]\n\
         skip_host_skill_discovery = {SKIP_HOST_SKILL_DISCOVERY}\n"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomProviderConfigError {
    /// The Base URL uses a scheme other than `https://`, or `http://` to a
    /// host other than an explicit local development target.
    NonHttpsBaseUrl,
    /// The Base URL carries embedded credentials
    /// (`https://user:pass@host/...` or `https://user@host/...`).
    CredentialInUrl,
    /// `name`, `model_id` or the Base URL is empty (`name`/`model_id` only)
    /// or contains a control character (including a newline), which would
    /// otherwise let a crafted value break out of the generated TOML string
    /// literal it is embedded in.
    InvalidField(&'static str),
}

impl std::fmt::Display for CustomProviderConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CustomProviderConfigError::NonHttpsBaseUrl => {
                write!(
                    f,
                    "Custom Provider base URL must use https:// (or http:// to an explicit local host)"
                )
            }
            CustomProviderConfigError::CredentialInUrl => {
                write!(f, "Custom Provider base URL must not embed credentials")
            }
            CustomProviderConfigError::InvalidField(field) => {
                write!(
                    f,
                    "Custom Provider {field} is empty or contains a control character"
                )
            }
        }
    }
}

impl std::error::Error for CustomProviderConfigError {}

/// Selects which `shell_environment_policy` TOML shape to generate for
/// excluding [`CUSTOM_PROVIDER_ENV_KEY`] from subprocesses the App Server
/// itself spawns (shell tool calls). Per ADR-0032 section 10, the public
/// Config Reference documents `filters` as current and `exclude`/
/// `include_only` as an older form; which one the bundled 0.157.1 binary
/// actually accepts under `--strict-config` must be confirmed against that
/// binary (see the crate's `tests/` integration test), not assumed from the
/// public docs alone. `Filters` is the default pending that confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShellEnvironmentPolicyFormat {
    #[default]
    Filters,
    LegacyExcludeList,
}

fn is_local_dev_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Extracts the bare hostname from an authority component (`host`,
/// `host:port`, or a bracketed IPv6 literal with an optional port), with no
/// userinfo (callers reject an authority containing `@` before this runs).
fn extract_host(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    authority.split(':').next().unwrap_or(authority)
}

/// Validates a Custom Provider Base URL per the rules in this module's
/// documentation. Does not attempt full RFC 3986 parsing: it only extracts
/// enough of the authority component to check for embedded credentials and
/// to allow the explicit local-development HTTP exception.
pub fn validate_base_url(raw: &str) -> Result<(), CustomProviderConfigError> {
    if raw.chars().any(|c| c.is_control()) {
        // Rejected before any scheme/authority parsing: a control character
        // (including `\n`/`\r`) here could otherwise inject a line into the
        // generated TOML once embedded in a string literal, regardless of
        // which branch below would otherwise accept the URL.
        return Err(CustomProviderConfigError::InvalidField("base_url"));
    }
    let (is_https, after_scheme) = if let Some(rest) = raw.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = raw.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(CustomProviderConfigError::NonHttpsBaseUrl);
    };

    let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err(CustomProviderConfigError::CredentialInUrl);
    }

    if !is_https && !is_local_dev_host(extract_host(authority)) {
        return Err(CustomProviderConfigError::NonHttpsBaseUrl);
    }
    Ok(())
}

fn validate_toml_string_field(
    value: &str,
    field: &'static str,
) -> Result<(), CustomProviderConfigError> {
    if value.is_empty() || value.chars().any(|c| c.is_control()) {
        return Err(CustomProviderConfigError::InvalidField(field));
    }
    Ok(())
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Generates the Codex config TOML selecting the Custom Provider connection.
/// The returned string never contains the API key: only [`CUSTOM_PROVIDER_ENV_KEY`],
/// the environment variable name the App Server itself reads the key from at
/// its own startup.
pub fn generate_custom_provider_toml(
    name: &str,
    base_url: &str,
    model_id: &str,
    shell_env_format: ShellEnvironmentPolicyFormat,
) -> Result<String, CustomProviderConfigError> {
    validate_base_url(base_url)?;
    validate_toml_string_field(name, "name")?;
    validate_toml_string_field(model_id, "model_id")?;

    let escaped_name = escape_toml_string(name);
    let escaped_base_url = escape_toml_string(base_url);
    let escaped_model = escape_toml_string(model_id);

    let shell_policy = match shell_env_format {
        ShellEnvironmentPolicyFormat::Filters => format!(
            "[shell_environment_policy]\nignore_default_excludes = false\n\n[shell_environment_policy.filters]\n\"{CUSTOM_PROVIDER_ENV_KEY}\" = \"exclude\"\n"
        ),
        ShellEnvironmentPolicyFormat::LegacyExcludeList => format!(
            "[shell_environment_policy]\nignore_default_excludes = false\nexclude = [\"{CUSTOM_PROVIDER_ENV_KEY}\"]\n"
        ),
    };

    Ok(format!(
        "model_provider = \"{CUSTOM_PROVIDER_ID}\"\n\
         model = \"{escaped_model}\"\n\
         project_doc_max_bytes = {PROJECT_DOC_MAX_BYTES}\n\
         project_root_markers = {PROJECT_ROOT_MARKERS}\n\
         \n\
         [features]\n\
         skip_host_skill_discovery = {SKIP_HOST_SKILL_DISCOVERY}\n\
         \n\
         [model_providers.{CUSTOM_PROVIDER_ID}]\n\
         name = \"{escaped_name}\"\n\
         base_url = \"{escaped_base_url}\"\n\
         wire_api = \"responses\"\n\
         env_key = \"{CUSTOM_PROVIDER_ENV_KEY}\"\n\
         requires_openai_auth = false\n\
         \n\
         {shell_policy}"
    ))
}

/// Everything needed to start the Custom connection's App Server child:
/// the generated config (to be written to its own `CODEX_HOME`) and the
/// environment to inject solely into that one child process.
pub struct CustomProviderMaterial {
    pub config_toml: String,
    pub extra_env: Vec<(String, String)>,
}

impl std::fmt::Debug for CustomProviderMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted_env: Vec<(&str, &str)> = self
            .extra_env
            .iter()
            .map(|(k, _)| (k.as_str(), "<redacted>"))
            .collect();
        f.debug_struct("CustomProviderMaterial")
            .field("config_toml", &self.config_toml)
            .field("extra_env", &redacted_env)
            .finish()
    }
}

/// Builds the generated config and child environment for one Custom
/// Provider connection attempt. `secret` is the API key retrieved from the
/// OS credential store for this connection's `CredentialRef`; it is placed
/// only in `extra_env`, never in `config_toml`.
pub fn build_custom_provider_material(
    name: &str,
    base_url: &str,
    model_id: &str,
    secret: &str,
    shell_env_format: ShellEnvironmentPolicyFormat,
) -> Result<CustomProviderMaterial, CustomProviderConfigError> {
    let config_toml = generate_custom_provider_toml(name, base_url, model_id, shell_env_format)?;
    Ok(CustomProviderMaterial {
        config_toml,
        extra_env: vec![(CUSTOM_PROVIDER_ENV_KEY.to_string(), secret.to_string())],
    })
}

/// The outcome of a failed [`write_codex_config`]: mirrors
/// [`AtomicWriteError`] (see [`crate::settings::SaveError::PersistedDurabilityUnconfirmed`]
/// for the same distinction applied to the `AiSettings` file) so callers can
/// tell a write that never took effect apart from one whose `rename` already
/// landed — `config.toml` may already reference the new Base URL/model/env
/// key — but whose parent-directory crash-durability fsync could not be
/// confirmed. Callers must not treat the latter like the former (e.g. by
/// deleting a credential the already-visible new config may now reference).
#[derive(Debug)]
pub enum WriteCodexConfigError {
    /// The write never took effect: `config.toml` (if it existed) still has
    /// its previous contents, or `codex_home` itself could not be created.
    Io(io::Error),
    /// The atomic replace's `rename` already landed, but the parent
    /// directory's own crash-durability fsync could not be confirmed.
    PersistedDurabilityUnconfirmed(io::Error),
}

impl std::fmt::Display for WriteCodexConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteCodexConfigError::Io(e) => {
                write!(f, "I/O error writing Custom Provider config: {e}")
            }
            WriteCodexConfigError::PersistedDurabilityUnconfirmed(e) => write!(
                f,
                "Custom Provider config replace may have already taken effect, but its crash-durability \
                 could not be confirmed: {e}"
            ),
        }
    }
}

impl std::error::Error for WriteCodexConfigError {}

/// Writes the generated config to `<codex_home>/config.toml` via an atomic
/// same-filesystem replace, so a reader (including the App Server itself, if
/// it were ever started concurrently with a regeneration) never observes a
/// torn file.
pub fn write_codex_config(
    codex_home: &Path,
    config_toml: &str,
) -> Result<(), WriteCodexConfigError> {
    std::fs::create_dir_all(codex_home).map_err(WriteCodexConfigError::Io)?;
    match atomic_write_bytes(&codex_home.join("config.toml"), config_toml.as_bytes()) {
        Ok(()) => Ok(()),
        Err(AtomicWriteError::NotPersisted(e)) => Err(WriteCodexConfigError::Io(e)),
        Err(AtomicWriteError::RenameSucceededSyncFailed(e)) => {
            Err(WriteCodexConfigError::PersistedDurabilityUnconfirmed(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_base_url_is_accepted() {
        assert!(validate_base_url("https://provider.example/v1").is_ok());
    }

    #[test]
    fn plain_http_to_a_non_local_host_is_rejected() {
        assert_eq!(
            validate_base_url("http://provider.example/v1"),
            Err(CustomProviderConfigError::NonHttpsBaseUrl)
        );
    }

    #[test]
    fn plain_http_to_localhost_or_loopback_is_accepted_for_local_dev() {
        assert!(validate_base_url("http://localhost:8080/v1").is_ok());
        assert!(validate_base_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_base_url("http://[::1]:8080/v1").is_ok());
    }

    #[test]
    fn a_newline_or_other_control_character_in_the_base_url_is_rejected() {
        assert_eq!(
            validate_base_url("https://provider.example/v1\nInjected = true"),
            Err(CustomProviderConfigError::InvalidField("base_url"))
        );
        assert_eq!(
            validate_base_url("https://provider.example/v1\r\n[malicious]"),
            Err(CustomProviderConfigError::InvalidField("base_url"))
        );
        assert_eq!(
            validate_base_url("https://provider.example/\u{0007}v1"),
            Err(CustomProviderConfigError::InvalidField("base_url"))
        );
    }

    #[test]
    fn non_http_scheme_is_rejected() {
        assert_eq!(
            validate_base_url("ftp://provider.example/v1"),
            Err(CustomProviderConfigError::NonHttpsBaseUrl)
        );
    }

    #[test]
    fn credential_in_url_is_rejected_for_https_and_local_http() {
        assert_eq!(
            validate_base_url("https://user:pass@provider.example/v1"),
            Err(CustomProviderConfigError::CredentialInUrl)
        );
        assert_eq!(
            validate_base_url("https://token@provider.example/v1"),
            Err(CustomProviderConfigError::CredentialInUrl)
        );
        assert_eq!(
            validate_base_url("http://user:pass@localhost/v1"),
            Err(CustomProviderConfigError::CredentialInUrl)
        );
    }

    #[test]
    fn generated_toml_never_contains_the_secret_and_only_references_the_env_key() {
        let material = build_custom_provider_material(
            "My Provider",
            "https://provider.example/v1",
            "gpt-test-model",
            "sk-super-secret-value",
            ShellEnvironmentPolicyFormat::Filters,
        )
        .unwrap();

        assert!(!material.config_toml.contains("sk-super-secret-value"));
        assert!(material.config_toml.contains(CUSTOM_PROVIDER_ENV_KEY));
        assert_eq!(
            material.extra_env,
            vec![(
                CUSTOM_PROVIDER_ENV_KEY.to_string(),
                "sk-super-secret-value".to_string()
            )]
        );
    }

    #[test]
    fn generated_toml_is_well_formed_and_has_expected_keys() {
        let toml_text = generate_custom_provider_toml(
            "My Provider",
            "https://provider.example/v1",
            "gpt-test-model",
            ShellEnvironmentPolicyFormat::Filters,
        )
        .unwrap();

        let parsed: toml::Value = toml_text
            .parse()
            .expect("generated config must be valid TOML");
        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some(CUSTOM_PROVIDER_ID)
        );
        assert_eq!(
            parsed.get("model").and_then(|v| v.as_str()),
            Some("gpt-test-model")
        );
        assert_eq!(
            parsed
                .get("project_doc_max_bytes")
                .and_then(|v| v.as_integer()),
            Some(0)
        );
        assert!(
            parsed
                .get("project_root_markers")
                .and_then(|v| v.as_array())
                .is_some_and(Vec::is_empty)
        );
        assert_eq!(
            parsed
                .get("features")
                .and_then(|features| features.get("skip_host_skill_discovery"))
                .and_then(toml::Value::as_bool),
            Some(true)
        );

        let provider = parsed
            .get("model_providers")
            .and_then(|v| v.get(CUSTOM_PROVIDER_ID))
            .expect("model_providers.hane_custom table must be present");
        assert_eq!(
            provider.get("name").and_then(|v| v.as_str()),
            Some("My Provider")
        );
        assert_eq!(
            provider.get("base_url").and_then(|v| v.as_str()),
            Some("https://provider.example/v1")
        );
        assert_eq!(
            provider.get("wire_api").and_then(|v| v.as_str()),
            Some("responses")
        );
        assert_eq!(
            provider.get("env_key").and_then(|v| v.as_str()),
            Some(CUSTOM_PROVIDER_ENV_KEY)
        );
        assert_eq!(
            provider
                .get("requires_openai_auth")
                .and_then(|v| v.as_bool()),
            Some(false)
        );

        let filters = parsed
            .get("shell_environment_policy")
            .and_then(|v| v.get("filters"))
            .expect("shell_environment_policy.filters table must be present");
        assert_eq!(
            filters
                .get(CUSTOM_PROVIDER_ENV_KEY)
                .and_then(|v| v.as_str()),
            Some("exclude")
        );
    }

    #[test]
    fn chatgpt_toml_uses_the_same_external_context_isolation_profile() {
        let parsed: toml::Value = generate_chatgpt_config_toml()
            .parse()
            .expect("ChatGPT config must be valid TOML");
        assert_eq!(
            parsed
                .get("project_doc_max_bytes")
                .and_then(|v| v.as_integer()),
            Some(0)
        );
        assert!(
            parsed
                .get("project_root_markers")
                .and_then(|v| v.as_array())
                .is_some_and(Vec::is_empty)
        );
        assert_eq!(
            parsed
                .get("features")
                .and_then(|features| features.get("skip_host_skill_discovery"))
                .and_then(toml::Value::as_bool),
            Some(true)
        );
        assert!(parsed.get("model_provider").is_none());
        assert!(parsed.get("model_providers").is_none());
    }

    #[test]
    fn legacy_exclude_list_format_is_also_well_formed() {
        let toml_text = generate_custom_provider_toml(
            "My Provider",
            "https://provider.example/v1",
            "gpt-test-model",
            ShellEnvironmentPolicyFormat::LegacyExcludeList,
        )
        .unwrap();
        let parsed: toml::Value = toml_text
            .parse()
            .expect("generated config must be valid TOML");
        let exclude_list = parsed
            .get("shell_environment_policy")
            .and_then(|v| v.get("exclude"))
            .and_then(|v| v.as_array())
            .expect("shell_environment_policy.exclude array must be present");
        assert_eq!(exclude_list.len(), 1);
        assert_eq!(exclude_list[0].as_str(), Some(CUSTOM_PROVIDER_ENV_KEY));
    }

    #[test]
    fn a_quote_or_backslash_in_the_display_name_cannot_break_out_of_the_toml_string() {
        let toml_text = generate_custom_provider_toml(
            "My \"Provider\" \\ co.",
            "https://provider.example/v1",
            "gpt-test-model",
            ShellEnvironmentPolicyFormat::Filters,
        )
        .unwrap();
        let parsed: toml::Value = toml_text
            .parse()
            .expect("generated config must still be valid TOML");
        let provider = parsed
            .get("model_providers")
            .and_then(|v| v.get(CUSTOM_PROVIDER_ID))
            .unwrap();
        assert_eq!(
            provider.get("name").and_then(|v| v.as_str()),
            Some("My \"Provider\" \\ co.")
        );
        // The injected quote must not have added a second, attacker-controlled
        // key to the table.
        assert_eq!(
            provider.get("wire_api").and_then(|v| v.as_str()),
            Some("responses")
        );
    }

    #[test]
    fn empty_or_control_character_fields_are_rejected_instead_of_generating_malformed_toml() {
        assert!(matches!(
            generate_custom_provider_toml(
                "",
                "https://provider.example/v1",
                "gpt",
                ShellEnvironmentPolicyFormat::Filters
            ),
            Err(CustomProviderConfigError::InvalidField("name"))
        ));
        assert!(matches!(
            generate_custom_provider_toml(
                "Name",
                "https://provider.example/v1",
                "",
                ShellEnvironmentPolicyFormat::Filters
            ),
            Err(CustomProviderConfigError::InvalidField("model_id"))
        ));
        assert!(matches!(
            generate_custom_provider_toml(
                "Name\nInjected = true",
                "https://provider.example/v1",
                "gpt",
                ShellEnvironmentPolicyFormat::Filters
            ),
            Err(CustomProviderConfigError::InvalidField("name"))
        ));
    }

    #[test]
    fn write_codex_config_persists_to_config_toml_under_codex_home() {
        let dir =
            std::env::temp_dir().join(format!("hane-ai-provider-test-{}", std::process::id()));
        let codex_home = dir.join("codex-custom");
        let toml_text = generate_custom_provider_toml(
            "My Provider",
            "https://provider.example/v1",
            "gpt-test-model",
            ShellEnvironmentPolicyFormat::Filters,
        )
        .unwrap();

        write_codex_config(&codex_home, &toml_text).unwrap();
        let written = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
        assert_eq!(written, toml_text);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn debug_formatting_of_the_material_never_prints_the_secret() {
        let material = build_custom_provider_material(
            "My Provider",
            "https://provider.example/v1",
            "gpt-test-model",
            "sk-super-secret-value",
            ShellEnvironmentPolicyFormat::Filters,
        )
        .unwrap();
        let formatted = format!("{material:?}");
        assert!(!formatted.contains("sk-super-secret-value"));
    }
}
