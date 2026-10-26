use std::fmt;
use std::sync::Arc;

use crate::child_env::ChildProxyPlan;
use crate::credential::{
    BasicCredentials, CredentialError, CredentialRef, CredentialSource, CredentialStore,
    load_credentials,
};
use crate::environment::{EnvPlatform, ProxyEnvironment};
use crate::error::NetworkError;
use crate::route::{Destination, ResolvedRoute, Scheme, resolve};
use crate::settings::NetworkSettings;

/// A credential store that can be shared between threads.
pub type SharedCredentialStore = Arc<dyn CredentialStore + Send + Sync>;

/// Stand-in used until an OS secret backend exists. Every lookup fails, so a
/// manual Basic proxy is reported as an error and never degrades to an
/// unauthenticated connection.
struct UnavailableStore;

impl CredentialStore for UnavailableStore {
    fn load(&self, _: &CredentialRef) -> Result<BasicCredentials, CredentialError> {
        Err(CredentialError::Unavailable)
    }
}

struct Inner {
    settings: NetworkSettings,
    environment: ProxyEnvironment,
    store: SharedCredentialStore,
}

/// One immutable network input shared by update checks and runtime downloads:
/// the settings, the proxy environment captured once by the caller, and the
/// credential store. Cloning shares the same snapshot, so one operation never
/// re-reads the environment between hops. It performs no I/O by itself.
#[derive(Clone)]
pub struct NetworkContext {
    inner: Arc<Inner>,
}

impl NetworkContext {
    pub fn new(
        settings: NetworkSettings,
        environment: ProxyEnvironment,
        store: SharedCredentialStore,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                settings,
                environment,
                store,
            }),
        }
    }

    /// Default (Environment) settings over a captured environment, with no
    /// secret backend.
    pub fn from_environment(environment: ProxyEnvironment) -> Self {
        Self::new(
            NetworkSettings::default(),
            environment,
            Arc::new(UnavailableStore),
        )
    }

    /// Ignore every proxy variable and connect directly.
    pub fn direct() -> Self {
        Self::new(
            NetworkSettings::direct(),
            ProxyEnvironment::from_vars(EnvPlatform::current(), std::iter::empty::<(&str, &str)>()),
            Arc::new(UnavailableStore),
        )
    }

    pub fn settings(&self) -> &NetworkSettings {
        &self.inner.settings
    }

    /// Route for `host` (no port) over this snapshot. Pure: no DNS, no
    /// connection, no secret access.
    pub fn route(&self, scheme: Scheme, host: &str) -> Result<ResolvedRoute, NetworkError> {
        let destination = Destination::new(scheme, host)?;
        resolve(&self.inner.settings, &self.inner.environment, &destination)
    }

    /// The proxy plan for a Hane-launched child process, built from this one
    /// snapshot (settings, captured environment, credential store). A
    /// credential or settings failure is an error: callers must not start the
    /// child without authentication or directly instead.
    pub fn child_proxy_plan(&self) -> Result<ChildProxyPlan, NetworkError> {
        ChildProxyPlan::from_settings(
            &self.inner.settings,
            &self.inner.environment,
            &*self.inner.store,
        )
    }

    /// Fetch the credentials of a resolved route and check that they can be
    /// carried as HTTP Basic. A store failure is an error, never `None`.
    pub fn credentials(
        &self,
        source: &CredentialSource,
    ) -> Result<Option<BasicCredentials>, NetworkError> {
        let loaded = load_credentials(source, &*self.inner.store)?;
        if let Some(credentials) = &loaded {
            credentials.check_for_basic()?;
        }
        Ok(loaded)
    }
}

impl Default for NetworkContext {
    /// Environment mode over an empty environment: no proxy, so direct. The
    /// application entry point passes the real captured environment instead.
    fn default() -> Self {
        Self::from_environment(ProxyEnvironment::from_vars(
            EnvPlatform::current(),
            std::iter::empty::<(&str, &str)>(),
        ))
    }
}

impl fmt::Debug for NetworkContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetworkContext")
            .field("mode", &self.inner.settings.mode)
            .field("environment", &self.inner.environment)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{ManualProxy, ProxyAuth};

    #[test]
    fn default_context_is_direct_without_variables() {
        let route = NetworkContext::default()
            .route(Scheme::Https, "api.github.com")
            .unwrap();
        assert_eq!(route, ResolvedRoute::Direct);
    }

    #[test]
    fn environment_snapshot_is_shared_by_clones() {
        let env = ProxyEnvironment::from_vars(
            EnvPlatform::MacOs,
            [("https_proxy", "http://u:pw@proxy.example:3128")],
        );
        let context = NetworkContext::from_environment(env);
        let clone = context.clone();
        let a = context.route(Scheme::Https, "github.com").unwrap();
        let b = clone.route(Scheme::Https, "github.com").unwrap();
        assert_eq!(a, b);
        assert!(matches!(a, ResolvedRoute::HttpProxy { .. }));
        assert!(!format!("{context:?}").contains("pw"));
    }

    #[test]
    fn child_plan_uses_the_captured_snapshot_only() {
        let env = ProxyEnvironment::from_vars(
            EnvPlatform::MacOs,
            [("https_proxy", "http://proxy.example:3128")],
        );
        let plan = NetworkContext::from_environment(env)
            .child_proxy_plan()
            .unwrap();
        assert!(plan.uses_proxy());
        let direct = NetworkContext::direct().child_proxy_plan().unwrap();
        assert!(!direct.uses_proxy());
    }

    #[test]
    fn child_plan_reports_credential_failure_instead_of_falling_back() {
        let manual = ManualProxy::new(
            "http://p:1",
            "",
            ProxyAuth::Basic(CredentialRef::new("c1").unwrap()),
        )
        .unwrap();
        let context = NetworkContext::new(
            NetworkSettings::manual(manual),
            ProxyEnvironment::from_vars(EnvPlatform::MacOs, std::iter::empty::<(&str, &str)>()),
            Arc::new(UnavailableStore),
        );
        assert_eq!(
            context.child_proxy_plan().err(),
            Some(NetworkError::Credential(CredentialError::Unavailable))
        );
    }

    #[test]
    fn unavailable_store_is_an_error_not_unauthenticated() {
        let reference = CredentialRef::new("c1").unwrap();
        let manual =
            ManualProxy::new("http://p:1", "", ProxyAuth::Basic(reference.clone())).unwrap();
        let context = NetworkContext::new(
            NetworkSettings::manual(manual),
            ProxyEnvironment::from_vars(EnvPlatform::MacOs, std::iter::empty::<(&str, &str)>()),
            Arc::new(UnavailableStore),
        );
        assert_eq!(
            context.credentials(&CredentialSource::Stored(reference)),
            Err(NetworkError::Credential(CredentialError::Unavailable))
        );
        assert_eq!(context.credentials(&CredentialSource::None), Ok(None));
    }

    #[test]
    fn credentials_unusable_as_basic_are_rejected() {
        let context = NetworkContext::default();
        for (user, pass) in [("a\r\nX: y", "p"), ("a", "p\0"), ("a:b", "p"), ("", "p")] {
            let source = CredentialSource::Inline(BasicCredentials::new(user, pass));
            assert_eq!(
                context.credentials(&source),
                Err(NetworkError::Credential(CredentialError::Invalid)),
                "{user:?}"
            );
        }
        let ok = CredentialSource::Inline(BasicCredentials::new("user", "p:\"\\ss"));
        assert!(context.credentials(&ok).unwrap().is_some());
    }
}
