//! The boundary between the connection threads and the editor's UI thread.

use crate::message::{ErrorCode, HostRequest, LinkError, LinkResult};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::Sender;

/// A millisecond clock that both processes share: on Windows
/// `GetTickCount64`, the time since boot. Deadlines are absolute values of it.
pub trait TickClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

const PENDING: u8 = 0;
const STARTED: u8 = 1;
const ABANDONED: u8 = 2;

/// Decides, exactly once, whether a request is applied: the UI thread claims
/// it right before touching the document, and the connection thread abandons
/// it when its deadline passes first. Whoever loses knows the other won, so a
/// timeout is reported as "not applied" only if it really never started.
#[derive(Debug, Default)]
pub struct Ticket {
    state: AtomicU8,
}

impl Ticket {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Claims the right to apply the request. Refused when the deadline has
    /// already passed or the connection side gave up.
    pub fn begin(&self, now_tick: u64, deadline_tick: Option<u64>) -> Result<(), LinkError> {
        if let Some(deadline) = deadline_tick
            && now_tick > deadline
        {
            // Mark it so the waiting side sees a definite outcome.
            let _ = self.state.compare_exchange(
                PENDING,
                ABANDONED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(LinkError::not_applied(ErrorCode::DeadlineExceeded));
        }
        self.state
            .compare_exchange(PENDING, STARTED, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| LinkError::not_applied(ErrorCode::DeadlineExceeded))
    }

    /// Gives up on a request the UI thread has not claimed. `true` means it
    /// will never be applied.
    pub fn abandon(&self) -> bool {
        match self
            .state
            .compare_exchange(PENDING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(state) => state == ABANDONED,
        }
    }

    pub fn started(&self) -> bool {
        self.state.load(Ordering::Acquire) == STARTED
    }
}

/// One request for the UI thread, with the way back.
pub struct HostCall {
    pub request: HostRequest,
    /// The service instance the request came in on.
    pub instance_id: String,
    pub ticket: Arc<Ticket>,
    pub reply: Sender<LinkResult>,
}

/// Why a call could not even be queued.
#[derive(Debug, Eq, PartialEq)]
pub enum SubmitError {
    /// The UI side is overloaded or gone; nothing was queued.
    Unavailable,
}

/// The UI side. `submit` must not block: it queues the call for the UI thread.
pub trait Host: Send + Sync {
    fn submit(&self, call: HostCall) -> Result<(), SubmitError>;
}

/// A [`Host`] that queues calls for the UI thread, which takes them with
/// `recv().await` on the receiver returned by [`QueueHost::new`].
pub struct QueueHost {
    sender: async_channel::Sender<HostCall>,
}

impl QueueHost {
    /// A queue of at most `capacity` waiting calls. When it is full, or the
    /// receiver is gone, `submit` fails and the request is refused as busy.
    pub fn new(capacity: usize) -> (Self, async_channel::Receiver<HostCall>) {
        let (sender, receiver) = async_channel::bounded(capacity);
        (Self { sender }, receiver)
    }
}

impl Host for QueueHost {
    fn submit(&self, call: HostCall) -> Result<(), SubmitError> {
        self.sender
            .try_send(call)
            .map_err(|_| SubmitError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_or_closed_queue_refuses_without_blocking() {
        let call = |ticket: Arc<Ticket>| {
            let (reply, _outcome) = std::sync::mpsc::channel();
            HostCall {
                request: HostRequest::ContextGet(crate::message::ContextGetParams {
                    hwnd: crate::message::WindowHandle(1),
                    before_chars: None,
                    after_chars: None,
                    expected_document_id: None,
                    expected_revision: None,
                    include_snapshot: false,
                    pending_range_handles: Vec::new(),
                }),
                instance_id: "i".to_owned(),
                ticket,
                reply,
            }
        };
        let (host, receiver) = QueueHost::new(1);
        assert_eq!(host.submit(call(Ticket::new())), Ok(()));
        assert_eq!(
            host.submit(call(Ticket::new())),
            Err(SubmitError::Unavailable)
        );
        drop(receiver);
        assert_eq!(
            host.submit(call(Ticket::new())),
            Err(SubmitError::Unavailable)
        );
    }

    #[test]
    fn begin_wins_once_and_abandon_loses_after_it() {
        let ticket = Ticket::new();
        assert!(ticket.begin(10, Some(20)).is_ok());
        assert!(ticket.started());
        assert!(!ticket.abandon());
        assert!(ticket.begin(10, Some(20)).is_err());
    }

    #[test]
    fn abandon_wins_before_begin() {
        let ticket = Ticket::new();
        assert!(ticket.abandon());
        assert_eq!(
            ticket.begin(1, Some(20)),
            Err(LinkError::not_applied(ErrorCode::DeadlineExceeded))
        );
        assert!(!ticket.started());
    }

    #[test]
    fn a_passed_deadline_blocks_the_claim() {
        let ticket = Ticket::new();
        assert!(ticket.begin(21, Some(20)).is_err());
        assert!(!ticket.started());
        assert!(ticket.abandon());
        // A claim without a deadline (context reads) is only a race check.
        let ticket = Ticket::new();
        assert!(ticket.begin(u64::MAX, None).is_ok());
    }
}
