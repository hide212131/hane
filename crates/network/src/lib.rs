//! Shared HTTP proxy settings and route resolution for Hane.
//!
//! This crate is a pure contract layer (Issue #623, design `docs/design/http-proxy.md`):
//! it never reads the process environment, resolves DNS, opens connections or
//! touches an OS secret store. Callers inject the environment map and a
//! [`CredentialStore`]. It does not depend on the AI, session, UI or update
//! crates. [`NetworkContext`] bundles the injected, immutable inputs that the
//! update and runtime-download callers share; it still does no I/O itself.

mod bypass;
mod child_env;
mod context;
mod credential;
mod endpoint;
mod environment;
mod error;
mod host;
mod route;
mod settings;

pub use bypass::BypassList;
pub use child_env::{ChildEnvironment, ChildProxyPlan};
pub use context::{NetworkContext, SharedCredentialStore};
pub use credential::{
    BasicCredentials, CredentialError, CredentialRef, CredentialSource, CredentialStore,
    PROXY_CREDENTIAL_SERVICE, Secret, load_credentials,
};
pub use endpoint::ProxyEndpoint;
pub use environment::{EnvPlatform, ProxyEnvironment};
pub use error::{BypassProblem, NetworkError, ProxySource, UrlProblem};
pub use host::Host;
pub use route::{Destination, ResolvedRoute, Scheme, resolve};
pub use settings::{ManualProxy, NetworkSettings, ProxyAuth, ProxyMode, SETTINGS_SCHEMA};
