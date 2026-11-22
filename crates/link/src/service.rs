//! The accept loop of one enabled period of the link.

use crate::message::{ErrorCode, LinkError};
use crate::server::{self, MAX_CONNECTIONS, Shared};
use crate::transport::{Connection, Deadlined, Listener, SLICE};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Instant;

/// A running service: accepts connections until [`RunningService::stop`].
pub struct RunningService {
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl RunningService {
    /// Starts accepting on `listener` on a background thread.
    pub fn start<L: Listener + 'static>(mut listener: L, shared: Arc<Shared>) -> Self {
        let loop_shared = Arc::clone(&shared);
        let accept = std::thread::Builder::new()
            .name("hane-link-accept".to_owned())
            .spawn(move || accept_loop(&mut listener, &loop_shared))
            .ok();
        Self { shared, accept }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// Stops accepting and tells every connection to end. Returns once the
    /// accept loop is gone; connection threads follow within one wait slice.
    pub fn stop(mut self) {
        self.stop_in_place();
    }

    fn stop_in_place(&mut self) {
        self.shared.stopping.store(true, Ordering::Release);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

impl Drop for RunningService {
    fn drop(&mut self) {
        self.stop_in_place();
    }
}

fn accept_loop<L: Listener>(listener: &mut L, shared: &Arc<Shared>) {
    while !shared.stopping.load(Ordering::Acquire) {
        let mut conn = match listener.accept(SLICE) {
            Ok(Some(conn)) => conn,
            Ok(None) => continue,
            Err(_) => {
                // A listener that fails is not retried in a tight loop.
                std::thread::sleep(SLICE);
                continue;
            }
        };
        if shared.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            shared.connections.fetch_sub(1, Ordering::AcqRel);
            refuse_over_capacity(&mut conn, shared);
            continue;
        }
        let shared = Arc::clone(shared);
        let spawned = std::thread::Builder::new()
            .name("hane-link-conn".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || {
                    let _ = server::serve_connection(&mut conn, &shared);
                    shared.connections.fetch_sub(1, Ordering::AcqRel);
                }
            });
        if spawned.is_err() {
            shared.connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

fn refuse_over_capacity<C: Connection>(conn: &mut C, shared: &Shared) {
    // Nothing is read from the peer and nothing about the document is said.
    let body = crate::message::error_response(
        &serde_json::Value::Null,
        LinkError::not_applied(ErrorCode::Busy),
    );
    let mut writer = Deadlined::new(
        conn,
        Instant::now() + std::time::Duration::from_secs(1),
        &shared.stopping,
    );
    let _ = crate::frame::write_frame(&mut writer, &body);
}
