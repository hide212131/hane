//! Fixed, document-free App Server connection probe.
//!
//! This module owns the versioned request profile and validates notification
//! correlation. It deliberately exposes only plain final text and classified
//! errors; raw RPC payloads and provider error strings never reach snapshots.

use std::collections::HashSet;
use std::path::Path;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::CUSTOM_PROVIDER_ID;
use crate::connect::{ConnectError, with_generation_checked_lock};
use crate::rpc::RpcError;
use crate::runtime::{AiRuntime, RuntimeError, RuntimeState};
use crate::runtime::{RuntimeEvent, RuntimeEventKind};
use crate::settings::ActiveConnection;
use crate::settings::AiSettingsStore;

pub const PROBE_PROFILE_VERSION: u32 = 1;
pub const PROBE_INPUT: &str = "Reply with exactly HANE_AI_OK.";
pub const MAX_PROBE_TEXT_BYTES: usize = 8 * 1024;
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(90);
const PROBE_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const INTERRUPT_TERMINAL_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_POLL: Duration = Duration::from_millis(20);

const PROBE_BASE_INSTRUCTIONS: &str =
    "This is a fixed connectivity check. Do not use tools. Reply briefly with plain text only.";
const PROBE_DEVELOPER_INSTRUCTIONS: &str = "Use only the fixed user input in this thread. Do not read files, browse, call tools, or use any other context.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeErrorCode {
    InvalidConfiguration,
    AccountUnavailable,
    ModelUnavailable,
    CredentialUnavailable,
    OwnedElsewhere,
    Busy,
    RuntimeUnavailable,
    StaleGeneration,
    Unauthorized,
    Forbidden,
    RateLimited,
    Network,
    TimedOut,
    ProtocolMismatch,
    NotificationOverflow,
    SafetyProfileUnsupported,
    ProviderFailed,
    Canceled,
    Isolated,
}

impl ProbeErrorCode {
    pub fn stable_code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "probe_invalid_configuration",
            Self::AccountUnavailable => "probe_account_unavailable",
            Self::ModelUnavailable => "probe_model_unavailable",
            Self::CredentialUnavailable => "probe_credential_unavailable",
            Self::OwnedElsewhere => "owned_elsewhere",
            Self::Busy => "busy",
            Self::RuntimeUnavailable => "runtime_unavailable",
            Self::StaleGeneration => "stale_generation",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::RateLimited => "rate_limited",
            Self::Network => "network_error",
            Self::TimedOut => "timed_out",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::NotificationOverflow => "notification_overflow",
            Self::SafetyProfileUnsupported => "safety_profile_unsupported",
            Self::ProviderFailed => "provider_failed",
            Self::Canceled => "canceled",
            Self::Isolated => "runtime_isolated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub text: String,
    pub connection: ActiveConnection,
    pub model: String,
    pub settings_generation: u64,
    pub runtime_generation: u64,
    pub auth_epoch: u64,
    pub profile_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeStatus {
    NotRun,
    Running,
    Succeeded,
    Failed(ProbeErrorCode),
    Canceled,
    TimedOut,
    Stale,
    Isolated,
}

pub fn thread_start_params(connection: ActiveConnection, model: &str, cwd: &Path) -> Value {
    let model_provider = match connection {
        ActiveConnection::ChatGpt => "openai",
        ActiveConnection::Custom => CUSTOM_PROVIDER_ID,
    };
    json!({
        "model": model,
        "modelProvider": model_provider,
        "cwd": cwd.to_string_lossy(),
        "ephemeral": true,
        "sandbox": "read-only",
        "approvalPolicy": "never",
        "baseInstructions": PROBE_BASE_INSTRUCTIONS,
        "developerInstructions": PROBE_DEVELOPER_INSTRUCTIONS,
    })
}

pub fn turn_start_params(thread_id: &str) -> Value {
    json!({
        "threadId": thread_id,
        "input": [{"type": "text", "text": PROBE_INPUT}],
    })
}

pub fn turn_interrupt_params(thread_id: &str, turn_id: &str) -> Value {
    json!({"threadId": thread_id, "turnId": turn_id})
}

pub fn thread_id_from_response(response: &Value) -> Result<String, ProbeErrorCode> {
    response
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or(ProbeErrorCode::ProtocolMismatch)
}

pub fn turn_id_from_response(response: &Value) -> Result<String, ProbeErrorCode> {
    response
        .get("turn")
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or(ProbeErrorCode::ProtocolMismatch)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeTerminal {
    Completed(String),
    Failed(ProbeErrorCode),
    Interrupted,
}

pub(crate) struct ProbeExecution {
    pub status: ProbeStatus,
    pub result: Option<ProbeResult>,
    pub isolated: bool,
    pub deferred_events: Vec<RuntimeEvent>,
}

struct ProbeRuntimeContext {
    connection: ActiveConnection,
    model: String,
    runtime_generation: u64,
}

impl ProbeExecution {
    fn failed(code: ProbeErrorCode) -> Self {
        Self {
            status: ProbeStatus::Failed(code),
            result: None,
            isolated: false,
            deferred_events: Vec::new(),
        }
    }
}

/// Runs one fixed-input request while holding the shared settings lock for
/// the complete thread/start → turn terminal/confirmed-stop interval.
#[allow(clippy::too_many_arguments)] // Worker context spans cancellation, ownership, and persistence guards.
pub(crate) fn execute_probe(
    store: &AiSettingsStore,
    runtime: &AiRuntime,
    events_rx: &Receiver<RuntimeEvent>,
    cancel_rx: &Receiver<crate::service::OperationId>,
    shutdown_rx: &Receiver<()>,
    shutdown_seen: &mut bool,
    operation_id: crate::service::OperationId,
    connection: ActiveConnection,
    model: String,
    cwd: &Path,
    settings_generation: u64,
    auth_epoch: u64,
) -> ProbeExecution {
    if cancellation_requested(cancel_rx, operation_id, shutdown_rx, shutdown_seen) {
        return ProbeExecution {
            status: ProbeStatus::Canceled,
            result: None,
            isolated: false,
            deferred_events: Vec::new(),
        };
    }
    let runtime_generation = runtime.snapshot().generation;
    let context = ProbeRuntimeContext {
        connection,
        model,
        runtime_generation,
    };
    let mut deferred_events = Vec::new();
    let result = with_generation_checked_lock(store, runtime, settings_generation, || {
        run_locked(
            runtime,
            events_rx,
            cancel_rx,
            shutdown_rx,
            operation_id,
            shutdown_seen,
            &context,
            cwd,
            &mut deferred_events,
        )
    });
    match result {
        Ok((status, isolated, text)) => {
            let result = text.map(|text| ProbeResult {
                text,
                connection,
                model: context.model,
                settings_generation,
                runtime_generation,
                auth_epoch,
                profile_version: PROBE_PROFILE_VERSION,
            });
            ProbeExecution {
                status,
                result,
                isolated,
                deferred_events,
            }
        }
        Err(ConnectError::SettingsBusy) => ProbeExecution::failed(ProbeErrorCode::Busy),
        Err(ConnectError::GenerationMismatch { .. }) => {
            ProbeExecution::failed(ProbeErrorCode::StaleGeneration)
        }
        Err(ConnectError::Runtime(RuntimeError::NotReady)) => {
            ProbeExecution::failed(ProbeErrorCode::RuntimeUnavailable)
        }
        Err(_) => ProbeExecution::failed(ProbeErrorCode::RuntimeUnavailable),
    }
}

#[allow(clippy::too_many_arguments)] // Keep cancellation and deferred-event channels explicit at this boundary.
fn run_locked(
    runtime: &AiRuntime,
    events_rx: &Receiver<RuntimeEvent>,
    cancel_rx: &Receiver<crate::service::OperationId>,
    shutdown_rx: &Receiver<()>,
    operation_id: crate::service::OperationId,
    shutdown_seen: &mut bool,
    context: &ProbeRuntimeContext,
    cwd: &Path,
    deferred_events: &mut Vec<RuntimeEvent>,
) -> (ProbeStatus, bool, Option<String>) {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let thread_response = match runtime.call(
        "thread/start",
        Some(thread_start_params(context.connection, &context.model, cwd)),
        bounded_rpc_timeout(deadline),
    ) {
        Ok(response) => response,
        Err(error) => return settle_rpc_failure(runtime, error, deferred_events),
    };
    let thread_id = match thread_id_from_response(&thread_response) {
        Ok(id) => id,
        Err(code) => return stop_for_uncertain_turn(runtime, code, deferred_events),
    };
    if cancellation_requested(cancel_rx, operation_id, shutdown_rx, shutdown_seen) {
        return (ProbeStatus::Canceled, false, None);
    }

    let turn_response = match runtime.call(
        "turn/start",
        Some(turn_start_params(&thread_id)),
        bounded_rpc_timeout(deadline),
    ) {
        Ok(response) => response,
        Err(error) => return settle_rpc_failure(runtime, error, deferred_events),
    };
    let turn_id = match turn_id_from_response(&turn_response) {
        Ok(id) => id,
        Err(code) => return stop_for_uncertain_turn(runtime, code, deferred_events),
    };
    let mut collector = ProbeCollector::new(
        context.runtime_generation,
        thread_id.clone(),
        turn_id.clone(),
    );

    loop {
        if let Some(terminal) = drain_probe_events(events_rx, &mut collector, deferred_events) {
            return settle_terminal(terminal);
        }
        if collector.has_unsafe_tool_activity() {
            return interrupt_or_stop(
                runtime,
                events_rx,
                &mut collector,
                &thread_id,
                &turn_id,
                deferred_events,
                ProbeErrorCode::SafetyProfileUnsupported,
            );
        }
        if cancellation_requested(cancel_rx, operation_id, shutdown_rx, shutdown_seen) {
            return interrupt_or_stop(
                runtime,
                events_rx,
                &mut collector,
                &thread_id,
                &turn_id,
                deferred_events,
                ProbeErrorCode::Canceled,
            );
        }
        let runtime_status = runtime.snapshot();
        if runtime_status.state != RuntimeState::Ready
            || runtime_status.generation != context.runtime_generation
        {
            let code = if runtime_status.restart_blocked {
                ProbeErrorCode::Isolated
            } else {
                ProbeErrorCode::RuntimeUnavailable
            };
            return stop_for_uncertain_turn(runtime, code, deferred_events);
        }
        if Instant::now() >= deadline {
            return interrupt_or_stop(
                runtime,
                events_rx,
                &mut collector,
                &thread_id,
                &turn_id,
                deferred_events,
                ProbeErrorCode::TimedOut,
            );
        }
        match events_rx.recv_timeout(EVENT_POLL) {
            Ok(event) => {
                if let Some(terminal) = observe_or_defer(event, &mut collector, deferred_events) {
                    return settle_terminal(terminal);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return stop_for_uncertain_turn(
                    runtime,
                    ProbeErrorCode::NotificationOverflow,
                    deferred_events,
                );
            }
        }
    }
}

fn bounded_rpc_timeout(deadline: Instant) -> Duration {
    PROBE_RPC_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()))
}

fn settle_rpc_failure(
    runtime: &AiRuntime,
    error: RuntimeError,
    deferred_events: &mut Vec<RuntimeEvent>,
) -> (ProbeStatus, bool, Option<String>) {
    let code = match error {
        RuntimeError::Rpc(RpcError::Timeout) => ProbeErrorCode::TimedOut,
        RuntimeError::Rpc(RpcError::Backpressure) => ProbeErrorCode::Busy,
        RuntimeError::Rpc(RpcError::Remote(_)) => ProbeErrorCode::ProviderFailed,
        RuntimeError::Rpc(RpcError::Disconnected) => ProbeErrorCode::Network,
        RuntimeError::RestartBlocked => return (ProbeStatus::Isolated, true, None),
        _ => ProbeErrorCode::RuntimeUnavailable,
    };
    let ambiguous = matches!(
        code,
        ProbeErrorCode::TimedOut | ProbeErrorCode::Network | ProbeErrorCode::ProtocolMismatch
    );
    if ambiguous {
        stop_for_uncertain_turn(runtime, code, deferred_events)
    } else {
        (ProbeStatus::Failed(code), false, None)
    }
}

fn stop_for_uncertain_turn(
    runtime: &AiRuntime,
    code: ProbeErrorCode,
    _deferred_events: &mut Vec<RuntimeEvent>,
) -> (ProbeStatus, bool, Option<String>) {
    outcome_after_stop(runtime.stop(), code)
}

fn outcome_after_stop(
    result: Result<crate::runtime::RuntimeStatus, RuntimeError>,
    code: ProbeErrorCode,
) -> (ProbeStatus, bool, Option<String>) {
    if result.is_ok_and(stop_confirmed) {
        (status_for_error(code), false, None)
    } else {
        (ProbeStatus::Isolated, true, None)
    }
}

fn stop_confirmed(status: crate::runtime::RuntimeStatus) -> bool {
    status.state == RuntimeState::Stopped
        || (status.state == RuntimeState::Failed && !status.restart_blocked)
}

fn status_for_error(code: ProbeErrorCode) -> ProbeStatus {
    match code {
        ProbeErrorCode::Canceled => ProbeStatus::Canceled,
        ProbeErrorCode::TimedOut => ProbeStatus::TimedOut,
        other => ProbeStatus::Failed(other),
    }
}

fn settle_terminal(terminal: ProbeTerminal) -> (ProbeStatus, bool, Option<String>) {
    match terminal {
        ProbeTerminal::Completed(text) => (ProbeStatus::Succeeded, false, Some(text)),
        ProbeTerminal::Failed(code) => (ProbeStatus::Failed(code), false, None),
        ProbeTerminal::Interrupted => (ProbeStatus::Canceled, false, None),
    }
}

fn interrupt_or_stop(
    runtime: &AiRuntime,
    events_rx: &Receiver<RuntimeEvent>,
    collector: &mut ProbeCollector,
    thread_id: &str,
    turn_id: &str,
    deferred_events: &mut Vec<RuntimeEvent>,
    error_code: ProbeErrorCode,
) -> (ProbeStatus, bool, Option<String>) {
    // An interrupt acknowledgement is not terminal. Keep the shared lock
    // until a matching terminal notification or confirmed child stop.
    let deadline = Instant::now() + INTERRUPT_TERMINAL_TIMEOUT;
    let _ = runtime.call(
        "turn/interrupt",
        Some(turn_interrupt_params(thread_id, turn_id)),
        INTERRUPT_TERMINAL_TIMEOUT.min(deadline.saturating_duration_since(Instant::now())),
    );
    while Instant::now() < deadline {
        if let Some(_terminal) = drain_probe_events(events_rx, collector, deferred_events) {
            return (status_for_error(error_code), false, None);
        }
        if let Some(_terminal) = events_rx
            .recv_timeout(EVENT_POLL)
            .ok()
            .and_then(|event| observe_or_defer(event, collector, deferred_events))
        {
            return (status_for_error(error_code), false, None);
        }
    }
    stop_for_uncertain_turn(runtime, error_code, deferred_events)
}

fn drain_probe_events(
    events_rx: &Receiver<RuntimeEvent>,
    collector: &mut ProbeCollector,
    deferred_events: &mut Vec<RuntimeEvent>,
) -> Option<ProbeTerminal> {
    loop {
        match events_rx.try_recv() {
            Ok(event) => {
                if let Some(terminal) = observe_or_defer(event, collector, deferred_events) {
                    return Some(terminal);
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return None,
        }
    }
}

fn observe_or_defer(
    event: RuntimeEvent,
    collector: &mut ProbeCollector,
    deferred_events: &mut Vec<RuntimeEvent>,
) -> Option<ProbeTerminal> {
    let keep_for_service = matches!(
        &event.kind,
        RuntimeEventKind::Notification { method, .. }
            if matches!(method.as_str(), "account/updated" | "account/login/completed")
    );
    let terminal = collector.observe(event.clone());
    if keep_for_service && terminal.is_none() {
        deferred_events.push(event);
    }
    terminal
}

fn cancellation_requested(
    cancel_rx: &Receiver<crate::service::OperationId>,
    operation_id: crate::service::OperationId,
    shutdown_rx: &Receiver<()>,
    shutdown_seen: &mut bool,
) -> bool {
    let shutdown_requested = shutdown_rx.try_recv().is_ok();
    *shutdown_seen |= shutdown_requested;
    let mut requested = shutdown_requested;
    loop {
        match cancel_rx.try_recv() {
            Ok(target) if target == operation_id => requested = true,
            Ok(_) => {}
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
    requested
}

/// Collects only completed agent messages belonging to the exact runtime,
/// thread, and turn. Item IDs and output size are bounded.
pub struct ProbeCollector {
    runtime_generation: u64,
    thread_id: String,
    turn_id: String,
    item_ids: HashSet<String>,
    text: String,
    text_bytes: usize,
    terminal: Option<ProbeTerminal>,
    unsafe_tool_activity: bool,
}

impl ProbeCollector {
    pub fn new(runtime_generation: u64, thread_id: String, turn_id: String) -> Self {
        Self {
            runtime_generation,
            thread_id,
            turn_id,
            item_ids: HashSet::new(),
            text: String::new(),
            text_bytes: 0,
            terminal: None,
            unsafe_tool_activity: false,
        }
    }

    pub fn observe(&mut self, event: RuntimeEvent) -> Option<ProbeTerminal> {
        if self.terminal.is_some() || event.generation != self.runtime_generation {
            return self.terminal.clone();
        }
        let RuntimeEventKind::Notification { method, params } = event.kind else {
            return None;
        };
        let params = params.unwrap_or(Value::Null);
        if params.get("threadId").and_then(Value::as_str) != Some(self.thread_id.as_str()) {
            return None;
        }

        let outcome = match method.as_str() {
            "item/started" | "item/completed" => {
                let item = params.get("item").unwrap_or(&Value::Null);
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                if params.get("turnId").and_then(Value::as_str) != Some(self.turn_id.as_str()) {
                    return None;
                }
                if is_tool_activity(item_type) {
                    self.unsafe_tool_activity = true;
                    None
                } else if method == "item/completed" && item_type == "agentMessage" {
                    let Some(id) = item
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                    else {
                        return Some(self.set_terminal(ProbeTerminal::Failed(
                            ProbeErrorCode::ProtocolMismatch,
                        )));
                    };
                    if self.item_ids.insert(id.to_owned())
                        && let Some(text) = item.get("text").and_then(Value::as_str)
                    {
                        self.append_bounded(text);
                    }
                    None
                } else {
                    None
                }
            }
            "turn/completed" | "turn/failed" => {
                let turn = params.get("turn").unwrap_or(&Value::Null);
                if turn.get("id").and_then(Value::as_str) != Some(self.turn_id.as_str()) {
                    return None;
                }
                let status = turn.get("status").and_then(Value::as_str).unwrap_or("");
                let error = turn.get("error").filter(|error| !error.is_null());
                if self.unsafe_tool_activity {
                    Some(ProbeTerminal::Failed(
                        ProbeErrorCode::SafetyProfileUnsupported,
                    ))
                } else if status == "interrupted" || status == "canceled" {
                    Some(ProbeTerminal::Interrupted)
                } else if method == "turn/completed" && status == "completed" && error.is_none() {
                    if self.text.trim().is_empty() {
                        Some(ProbeTerminal::Failed(ProbeErrorCode::ProtocolMismatch))
                    } else {
                        Some(ProbeTerminal::Completed(self.text.clone()))
                    }
                } else if error.is_some() || status == "failed" || method == "turn/failed" {
                    Some(ProbeTerminal::Failed(error.map_or(
                        ProbeErrorCode::ProviderFailed,
                        classify_provider_error,
                    )))
                } else {
                    Some(ProbeTerminal::Failed(ProbeErrorCode::ProtocolMismatch))
                }
            }
            _ => None,
        };
        outcome.map(|terminal| self.set_terminal(terminal))
    }

    pub fn has_unsafe_tool_activity(&self) -> bool {
        self.unsafe_tool_activity
    }

    fn append_bounded(&mut self, value: &str) {
        let remaining = MAX_PROBE_TEXT_BYTES.saturating_sub(self.text_bytes);
        if remaining == 0 {
            return;
        }
        let mut end = value.len().min(remaining);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&value[..end]);
        self.text_bytes = self.text_bytes.saturating_add(end);
    }

    fn set_terminal(&mut self, terminal: ProbeTerminal) -> ProbeTerminal {
        self.terminal = Some(terminal.clone());
        terminal
    }
}

fn is_tool_activity(item_type: &str) -> bool {
    matches!(
        item_type,
        "commandExecution"
            | "fileChange"
            | "webSearch"
            | "mcpToolCall"
            | "collabAgentToolCall"
            | "imageGeneration"
            | "browser"
            | "computerAction"
    ) || item_type.to_ascii_lowercase().contains("tool")
}

fn classify_provider_error(error: &Value) -> ProbeErrorCode {
    fn scan(value: &Value) -> Option<u16> {
        match value {
            Value::Object(fields) => {
                for key in ["httpStatusCode", "statusCode", "status"] {
                    if let Some(status) = fields.get(key).and_then(Value::as_u64)
                        && (100..=599).contains(&status)
                    {
                        return Some(status as u16);
                    }
                }
                fields.values().find_map(scan)
            }
            Value::Array(values) => values.iter().find_map(scan),
            _ => None,
        }
    }

    let info = error.get("codexErrorInfo");
    match info.and_then(Value::as_str) {
        Some("unauthorized") => return ProbeErrorCode::Unauthorized,
        Some("rateLimitExceeded") => return ProbeErrorCode::RateLimited,
        _ => {}
    }
    match scan(error) {
        Some(401) => ProbeErrorCode::Unauthorized,
        Some(403) => ProbeErrorCode::Forbidden,
        Some(429) => ProbeErrorCode::RateLimited,
        Some(_) => ProbeErrorCode::ProviderFailed,
        None => ProbeErrorCode::ProviderFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(generation: u64, method: &str, params: Value) -> RuntimeEvent {
        RuntimeEvent {
            generation,
            kind: RuntimeEventKind::Notification {
                method: method.to_owned(),
                params: Some(params),
            },
        }
    }

    fn new_collector() -> ProbeCollector {
        ProbeCollector::new(7, "thread-1".to_owned(), "turn-1".to_owned())
    }

    #[test]
    fn turn_start_ack_is_not_probe_success() {
        assert_eq!(
            thread_id_from_response(&json!({"thread":{"id":"t"}})),
            Ok("t".into())
        );
        assert_eq!(
            turn_id_from_response(&json!({"turn":{"id":"u"}})),
            Ok("u".into())
        );
        assert_eq!(
            turn_id_from_response(&json!({})),
            Err(ProbeErrorCode::ProtocolMismatch)
        );
    }

    #[test]
    fn completion_requires_matching_turn_terminal_and_nonempty_final_text() {
        let mut collector = new_collector();
        assert_eq!(
            collector.observe(event(
                7,
                "turn/completed",
                json!({
                    "threadId":"thread-1", "turn":{"id":"turn-1","status":"completed","error":null}
                })
            )),
            Some(ProbeTerminal::Failed(ProbeErrorCode::ProtocolMismatch))
        );

        let mut collector = new_collector();
        assert_eq!(collector.observe(event(7, "item/completed", json!({
            "threadId":"thread-1", "turnId":"turn-1", "item":{"id":"item-1","type":"agentMessage","text":"OK"}
        }))), None);
        assert_eq!(
            collector.observe(event(
                7,
                "turn/completed",
                json!({
                    "threadId":"thread-1", "turn":{"id":"turn-1","status":"completed","error":null}
                })
            )),
            Some(ProbeTerminal::Completed("OK".into()))
        );
    }

    #[test]
    fn duplicate_items_and_foreign_generations_threads_and_turns_are_ignored() {
        let mut collector = new_collector();
        for (generation, params) in [
            (
                6,
                json!({"threadId":"thread-1","turnId":"turn-1","item":{"id":"a","type":"agentMessage","text":"stale"}}),
            ),
            (
                7,
                json!({"threadId":"other","turnId":"turn-1","item":{"id":"b","type":"agentMessage","text":"foreign"}}),
            ),
            (
                7,
                json!({"threadId":"thread-1","turnId":"other","item":{"id":"c","type":"agentMessage","text":"foreign"}}),
            ),
            (
                7,
                json!({"threadId":"thread-1","turnId":"turn-1","item":{"id":"a","type":"agentMessage","text":"right"}}),
            ),
            (
                7,
                json!({"threadId":"thread-1","turnId":"turn-1","item":{"id":"a","type":"agentMessage","text":"duplicate"}}),
            ),
        ] {
            assert_eq!(
                collector.observe(event(generation, "item/completed", params)),
                None
            );
        }
        assert_eq!(
            collector.observe(event(
                7,
                "turn/completed",
                json!({
                    "threadId":"thread-1", "turn":{"id":"turn-1","status":"completed","error":null}
                })
            )),
            Some(ProbeTerminal::Completed("right".into()))
        );
    }

    #[test]
    fn provider_statuses_are_classified_without_copying_error_text() {
        for (status, expected) in [
            (401, ProbeErrorCode::Unauthorized),
            (403, ProbeErrorCode::Forbidden),
            (429, ProbeErrorCode::RateLimited),
        ] {
            let mut collector = new_collector();
            collector.observe(event(7, "item/completed", json!({
                "threadId":"thread-1", "turnId":"turn-1", "item":{"id":"a","type":"agentMessage","text":"partial"}
            })));
            assert_eq!(collector.observe(event(7, "turn/completed", json!({
                "threadId":"thread-1", "turn":{"id":"turn-1","status":"failed","error":{"message":"secret diagnostic","codexErrorInfo":{"httpConnectionFailed":{"httpStatusCode":status}}}}
            }))), Some(ProbeTerminal::Failed(expected)));
        }
    }

    #[test]
    fn tool_activity_never_becomes_a_successful_probe() {
        let mut collector = new_collector();
        assert_eq!(collector.observe(event(7, "item/started", json!({
            "threadId":"thread-1", "turnId":"turn-1", "item":{"id":"tool-1","type":"commandExecution"}
        }))), None);
        assert!(collector.has_unsafe_tool_activity());
        assert_eq!(
            collector.observe(event(
                7,
                "turn/completed",
                json!({
                    "threadId":"thread-1", "turn":{"id":"turn-1","status":"completed","error":null}
                })
            )),
            Some(ProbeTerminal::Failed(
                ProbeErrorCode::SafetyProfileUnsupported
            ))
        );
    }

    #[test]
    fn unconfirmed_stop_is_isolated_and_only_confirmed_terminal_status_releases_guard() {
        let blocked = crate::runtime::RuntimeStatus {
            state: RuntimeState::Failed,
            restart_blocked: true,
            generation: 7,
            operation_generation: 3,
        };
        let (status, isolated, text) = outcome_after_stop(Ok(blocked), ProbeErrorCode::TimedOut);
        assert_eq!(status, ProbeStatus::Isolated);
        assert!(isolated);
        assert!(text.is_none());

        let (status, isolated, text) = outcome_after_stop(
            Err(RuntimeError::CoordinatorUnavailable),
            ProbeErrorCode::Canceled,
        );
        assert_eq!(status, ProbeStatus::Isolated);
        assert!(isolated);
        assert!(text.is_none());

        let stopped = crate::runtime::RuntimeStatus {
            state: RuntimeState::Stopped,
            restart_blocked: false,
            generation: 8,
            operation_generation: 4,
        };
        let (status, isolated, text) = outcome_after_stop(Ok(stopped), ProbeErrorCode::Canceled);
        assert_eq!(status, ProbeStatus::Canceled);
        assert!(!isolated);
        assert!(text.is_none());
    }

    #[test]
    fn output_is_plain_text_and_bounded_on_utf8_boundaries() {
        let mut collector = new_collector();
        let body = "あ".repeat(MAX_PROBE_TEXT_BYTES / 3 + 10);
        collector.observe(event(7, "item/completed", json!({
            "threadId":"thread-1", "turnId":"turn-1", "item":{"id":"a","type":"agentMessage","text":body}
        })));
        let terminal = collector.observe(event(
            7,
            "turn/completed",
            json!({
                "threadId":"thread-1", "turn":{"id":"turn-1","status":"completed","error":null}
            }),
        ));
        let Some(ProbeTerminal::Completed(text)) = terminal else {
            panic!("expected a completed text result");
        };
        assert!(text.len() <= MAX_PROBE_TEXT_BYTES);
        assert!(text.len() >= MAX_PROBE_TEXT_BYTES - 2);
    }
}
