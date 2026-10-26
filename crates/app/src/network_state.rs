//! The application's current network snapshot (Issue #623).
//!
//! The proxy environment is captured once. A setting saved on the network
//! settings screen replaces the snapshot, so an operation that starts
//! afterwards uses it; an operation that already took a snapshot keeps it,
//! because [`NetworkContext`] is immutable and shared by clone.

use std::sync::{Arc, Mutex, PoisonError};

use hane_network::{NetworkContext, NetworkSettings, ProxyEnvironment, SharedCredentialStore};

pub(crate) struct NetworkState {
    environment: ProxyEnvironment,
    store: SharedCredentialStore,
    current: Mutex<NetworkContext>,
}

impl NetworkState {
    pub(crate) fn new(
        settings: NetworkSettings,
        environment: ProxyEnvironment,
        store: SharedCredentialStore,
    ) -> Self {
        let current = NetworkContext::new(settings, environment.clone(), Arc::clone(&store));
        Self {
            environment,
            store,
            current: Mutex::new(current),
        }
    }

    /// The snapshot for an operation that starts now.
    pub(crate) fn current(&self) -> NetworkContext {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Makes `settings` (over the same captured environment and secret store)
    /// the snapshot of every later operation. Performs no I/O.
    pub(crate) fn replace(&self, settings: NetworkSettings) -> NetworkContext {
        let next = NetworkContext::new(settings, self.environment.clone(), Arc::clone(&self.store));
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = next.clone();
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_network::{EnvPlatform, ResolvedRoute, Scheme};

    fn state(vars: &[(&str, &str)]) -> NetworkState {
        NetworkState::new(
            NetworkSettings::default(),
            ProxyEnvironment::from_vars(EnvPlatform::MacOs, vars.iter().copied()),
            Arc::new(hane_ui::OsProxyCredentialStore),
        )
    }

    #[test]
    fn a_saved_setting_applies_to_later_operations_and_not_to_a_taken_snapshot() {
        let state = state(&[("https_proxy", "http://proxy.example:3128")]);
        let in_flight = state.current();
        assert!(matches!(
            in_flight.route(Scheme::Https, "api.github.com").unwrap(),
            ResolvedRoute::HttpProxy { .. }
        ));

        state.replace(NetworkSettings::direct());

        // A new operation sees the saved setting without a restart.
        assert_eq!(
            state.current().route(Scheme::Https, "api.github.com").unwrap(),
            ResolvedRoute::Direct
        );
        // The operation that began earlier keeps its own settings.
        assert_eq!(*in_flight.settings(), NetworkSettings::default());
        assert!(matches!(
            in_flight.route(Scheme::Https, "api.github.com").unwrap(),
            ResolvedRoute::HttpProxy { .. }
        ));
    }
}
