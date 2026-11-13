//! Decision logic of the automatic ChatGPT connection preparation (Issue #724).
//!
//! The service worker owns the I/O and the busy gate; this module only decides
//! the next step from observed facts. It performs no I/O and reads no clock:
//! the caller passes the time, so the success cache is testable.
//!
//! One preparation runs account read, model catalog read, model decision (with
//! an automatic save of a first model), and the fixed probe. A failed stage
//! stops the run; nothing advances or loops on its own. A run starts only from
//! a permitted trigger (settings opened, login completed, a settings change
//! applied, an explicit retry) once the settings screen has been opened.

use std::time::{Duration, Instant};

use crate::models::{ChatGptModel, ModelSelection, select_chatgpt_model};
use crate::probe::PROBE_PROFILE_VERSION;
use crate::settings::ActiveConnection;

/// How long a successful probe is reused for the same [`ProbeKey`]. This is a
/// freshness/cost trade-off chosen for Issue #724, not a contract of the probe.
pub const PROBE_SUCCESS_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparationTrigger {
    /// The AI settings screen was opened.
    OpenSettings,
    /// A new settings revision was saved and applied.
    Applied,
    /// A login finished and its account/model reads were reconciled.
    LoginCompleted,
    /// The user asked to retry a stopped preparation.
    Retry,
}

impl PreparationTrigger {
    fn rank(self) -> u8 {
        match self {
            Self::OpenSettings => 0,
            Self::Applied => 1,
            Self::LoginCompleted => 2,
            Self::Retry => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// A new preparation is pending.
    Started,
    /// It joined the pending or running preparation; nothing was added.
    Joined,
    /// The settings screen was never opened, so nothing may start.
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparationStage {
    ReadingAccount,
    ReadingModels,
    SavingModel,
    Probing,
}

/// Why a preparation stopped. The action that resolves it differs per cause;
/// none of these carries provider text, a credential or a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparationFailure {
    /// Ownership, recovery or persistence state forbids new AI operations.
    NotReady,
    /// The account state could not be read.
    AccountReadFailed,
    /// Not signed in to ChatGPT.
    NeedsLogin,
    /// The model catalog could not be read.
    ModelListFailed,
    /// The catalog is empty, so no model can be chosen.
    NoModelCandidates,
    /// The saved model is not in the catalog; it is kept, not replaced.
    SavedModelMissing,
    /// The automatic model save failed.
    ModelSaveFailed,
    /// The probe failed, timed out or was refused; see `AiSnapshot::probe_status`.
    ProbeFailed,
}

impl PreparationFailure {
    pub fn stable_code(self) -> &'static str {
        match self {
            Self::NotReady => "preparation_not_ready",
            Self::AccountReadFailed => "preparation_account_read_failed",
            Self::NeedsLogin => "preparation_needs_login",
            Self::ModelListFailed => "preparation_model_list_failed",
            Self::NoModelCandidates => "preparation_no_model_candidates",
            Self::SavedModelMissing => "preparation_saved_model_missing",
            Self::ModelSaveFailed => "preparation_model_save_failed",
            Self::ProbeFailed => "preparation_probe_failed",
        }
    }
}

/// What the settings screen shows for the preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreparationStatus {
    #[default]
    Idle,
    Running(PreparationStage),
    Ready,
    Stopped(PreparationFailure),
    Canceled,
}

/// The configuration a stage result belongs to. A result is adopted only while
/// the connection, settings generation and network revision are unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageContext {
    pub connection: ActiveConnection,
    pub settings_generation: u64,
    pub runtime_generation: u64,
    pub network_revision: u64,
}

impl StageContext {
    /// Whether `other` is the same configuration. The runtime generation may
    /// legitimately advance while a stage starts the runtime, so it is excluded.
    pub fn same_configuration(&self, other: &StageContext) -> bool {
        self.connection == other.connection
            && self.settings_generation == other.settings_generation
            && self.network_revision == other.network_revision
    }
}

/// Identity of a probe result. Neither keys nor tokens are part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeKey {
    pub connection: ActiveConnection,
    pub settings_generation: u64,
    pub runtime_generation: u64,
    pub auth_epoch: u64,
    pub model: String,
    pub profile_version: u32,
    pub network_revision: u64,
}

impl ProbeKey {
    pub fn new(context: &StageContext, auth_epoch: u64, model: &str) -> Self {
        Self {
            connection: context.connection,
            settings_generation: context.settings_generation,
            runtime_generation: context.runtime_generation,
            auth_epoch,
            model: model.to_owned(),
            profile_version: PROBE_PROFILE_VERSION,
            network_revision: context.network_revision,
        }
    }

    /// The same connection condition for failure suppression. Runtime
    /// regeneration and notifications alone must not lift a suppression.
    fn same_attempt(&self, other: &ProbeKey) -> bool {
        self.connection == other.connection
            && self.settings_generation == other.settings_generation
            && self.model == other.model
            && self.profile_version == other.profile_version
            && self.network_revision == other.network_revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    Succeeded,
    /// Failed, canceled, timed out or refused.
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub enum ModelsFact<'a> {
    NotLoaded,
    Loaded(&'a [ChatGptModel]),
    Failed,
}

/// Everything [`ConnectionPreparation::next_step`] decides from.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    pub context: StageContext,
    pub auth_epoch: u64,
    /// Settings are owned, no recovery is pending and the runtime is not isolated.
    pub ready: bool,
    pub signed_in: bool,
    pub account_read_failed: bool,
    pub models: ModelsFact<'a>,
    pub saved_model: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The active connection is not prepared automatically (P1: ChatGPT only).
    Skip,
    ReadAccount,
    ReadModels,
    /// Persist this first model through the ordinary save path.
    SaveModel(String),
    Probe(ProbeKey),
    /// A successful probe for the same key is still fresh.
    Ready,
    Stop(PreparationFailure),
}

#[derive(Debug, Default)]
pub struct ConnectionPreparation {
    armed: bool,
    pending: Option<PreparationTrigger>,
    running: Option<PreparationTrigger>,
    carry: Option<(StageContext, bool)>,
    account_read: Option<StageContext>,
    models_read: Option<StageContext>,
    saved_this_run: bool,
    success: Option<(ProbeKey, Instant)>,
    failure: Option<ProbeKey>,
}

impl ConnectionPreparation {
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Records a trigger. Opening the settings and an explicit retry arm the
    /// machine; the other triggers are ignored until then, so application
    /// start and ordinary editing never prepare a connection.
    pub fn request(&mut self, trigger: PreparationTrigger) -> Admission {
        if matches!(
            trigger,
            PreparationTrigger::OpenSettings | PreparationTrigger::Retry
        ) {
            self.armed = true;
        }
        if !self.armed {
            return Admission::Ignored;
        }
        if self.running.is_some() {
            return Admission::Joined;
        }
        match self.pending {
            Some(existing) => {
                if trigger.rank() > existing.rank() {
                    self.pending = Some(trigger);
                }
                Admission::Joined
            }
            None => {
                self.pending = Some(trigger);
                Admission::Started
            }
        }
    }

    /// Remembers that a login reconciliation just read the account (and, when
    /// signed in, the model catalog) so the run it triggers does not repeat them.
    pub fn record_login_reconciliation(&mut self, context: StageContext, signed_in: bool) {
        self.carry = Some((context, signed_in));
    }

    /// Starts the pending preparation, if any and none is running.
    pub fn begin(&mut self) -> Option<PreparationTrigger> {
        if self.running.is_some() {
            return None;
        }
        let trigger = self.pending.take()?;
        self.running = Some(trigger);
        self.account_read = None;
        self.models_read = None;
        self.saved_this_run = false;
        let carry = self.carry.take();
        match trigger {
            PreparationTrigger::Retry | PreparationTrigger::LoginCompleted => {
                self.success = None;
                self.failure = None;
            }
            PreparationTrigger::Applied => self.failure = None,
            PreparationTrigger::OpenSettings => {}
        }
        if trigger == PreparationTrigger::LoginCompleted
            && let Some((context, signed_in)) = carry
        {
            self.account_read = Some(context);
            if signed_in {
                self.models_read = Some(context);
            }
        }
        Some(trigger)
    }

    pub fn finish_run(&mut self) {
        self.running = None;
    }

    pub fn record_account_read(&mut self, context: StageContext) {
        self.account_read = Some(context);
    }

    pub fn record_models_read(&mut self, context: StageContext) {
        self.models_read = Some(context);
    }

    pub fn record_model_saved(&mut self) {
        self.saved_this_run = true;
    }

    pub fn record_probe(&mut self, key: ProbeKey, outcome: ProbeOutcome, now: Instant) {
        match outcome {
            ProbeOutcome::Succeeded => {
                self.success = Some((key, now));
                self.failure = None;
            }
            ProbeOutcome::Failed => {
                self.success = None;
                self.failure = Some(key);
            }
        }
    }

    pub fn next_step(&mut self, facts: &Facts<'_>, now: Instant) -> Step {
        let context = facts.context;
        if context.connection != ActiveConnection::ChatGpt {
            return Step::Skip;
        }
        if !facts.ready {
            return Step::Stop(PreparationFailure::NotReady);
        }
        if self.account_read != Some(context) {
            return Step::ReadAccount;
        }
        if facts.account_read_failed {
            return Step::Stop(PreparationFailure::AccountReadFailed);
        }
        if !facts.signed_in {
            return Step::Stop(PreparationFailure::NeedsLogin);
        }
        if self.models_read != Some(context) {
            return Step::ReadModels;
        }
        let models = match facts.models {
            ModelsFact::Loaded(models) => models,
            ModelsFact::NotLoaded | ModelsFact::Failed => {
                return Step::Stop(PreparationFailure::ModelListFailed);
            }
        };
        let model = match select_chatgpt_model(None, facts.saved_model, models) {
            ModelSelection::UserChoice(model) | ModelSelection::Saved(model) => model,
            ModelSelection::SavedMissing(_) => {
                return Step::Stop(PreparationFailure::SavedModelMissing);
            }
            ModelSelection::Default(model) | ModelSelection::FirstCandidate(model) => {
                // One automatic save per run: a save that did not make the
                // model the saved one must stop instead of looping.
                return if self.saved_this_run {
                    Step::Stop(PreparationFailure::ModelSaveFailed)
                } else {
                    Step::SaveModel(model)
                };
            }
            ModelSelection::NoCandidates => {
                return Step::Stop(PreparationFailure::NoModelCandidates);
            }
        };
        let key = ProbeKey::new(&context, facts.auth_epoch, &model);
        if let Some((cached, at)) = &self.success
            && *cached == key
            && now.saturating_duration_since(*at) < PROBE_SUCCESS_TTL
        {
            return Step::Ready;
        }
        if let Some(failed) = &self.failure
            && failed.same_attempt(&key)
        {
            return Step::Stop(PreparationFailure::ProbeFailed);
        }
        Step::Probe(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(connection: ActiveConnection) -> StageContext {
        StageContext {
            connection,
            settings_generation: 1,
            runtime_generation: 1,
            network_revision: 0,
        }
    }

    fn model(model: &str, is_default: bool) -> ChatGptModel {
        ChatGptModel {
            id: format!("catalog-{model}"),
            model: model.to_owned(),
            display_name: format!("Display {model}"),
            is_default,
        }
    }

    fn facts<'a>(
        context: StageContext,
        models: ModelsFact<'a>,
        saved_model: Option<&'a str>,
    ) -> Facts<'a> {
        Facts {
            context,
            auth_epoch: 1,
            ready: true,
            signed_in: true,
            account_read_failed: false,
            models,
            saved_model,
        }
    }

    fn started(trigger: PreparationTrigger) -> ConnectionPreparation {
        let mut prep = ConnectionPreparation::default();
        assert_eq!(prep.request(trigger), Admission::Started);
        assert_eq!(prep.begin(), Some(trigger));
        prep
    }

    fn read_everything(prep: &mut ConnectionPreparation, context: StageContext) {
        prep.record_account_read(context);
        prep.record_models_read(context);
    }

    #[test]
    fn opening_settings_starts_active_connection_preparation() {
        let now = Instant::now();
        let mut unarmed = ConnectionPreparation::default();
        assert_eq!(
            unarmed.request(PreparationTrigger::LoginCompleted),
            Admission::Ignored,
            "nothing starts before the settings screen is opened"
        );
        assert!(!unarmed.has_pending());

        let mut prep = started(PreparationTrigger::OpenSettings);
        let models = [model("gpt-a", true)];
        let chatgpt = facts(
            context(ActiveConnection::ChatGpt),
            ModelsFact::Loaded(&models),
            None,
        );
        assert_eq!(prep.next_step(&chatgpt, now), Step::ReadAccount);

        let custom = facts(
            context(ActiveConnection::Custom),
            ModelsFact::NotLoaded,
            None,
        );
        assert_eq!(
            prep.next_step(&custom, now),
            Step::Skip,
            "only the active ChatGPT connection is prepared"
        );
    }

    #[test]
    fn duplicate_triggers_join_the_running_preparation() {
        let mut prep = ConnectionPreparation::default();
        assert_eq!(
            prep.request(PreparationTrigger::OpenSettings),
            Admission::Started
        );
        assert_eq!(
            prep.request(PreparationTrigger::OpenSettings),
            Admission::Joined
        );
        assert_eq!(
            prep.request(PreparationTrigger::Retry),
            Admission::Joined,
            "a stronger trigger upgrades the pending one instead of adding a run"
        );
        assert_eq!(prep.begin(), Some(PreparationTrigger::Retry));
        assert_eq!(prep.begin(), None, "one run at a time");
        assert_eq!(
            prep.request(PreparationTrigger::OpenSettings),
            Admission::Joined,
            "a trigger during a run joins it"
        );
        assert!(!prep.has_pending());
        prep.finish_run();
        assert_eq!(
            prep.request(PreparationTrigger::OpenSettings),
            Admission::Started
        );
    }

    #[test]
    fn login_reconciliation_continues_without_duplicate_reads() {
        let now = Instant::now();
        let context = context(ActiveConnection::ChatGpt);
        let models = [model("gpt-a", false), model("gpt-b", true)];

        let mut prep = ConnectionPreparation::default();
        prep.request(PreparationTrigger::OpenSettings);
        prep.begin();
        prep.finish_run();

        prep.record_login_reconciliation(context, true);
        prep.request(PreparationTrigger::LoginCompleted);
        assert_eq!(prep.begin(), Some(PreparationTrigger::LoginCompleted));
        assert_eq!(
            prep.next_step(&facts(context, ModelsFact::Loaded(&models), None), now),
            Step::SaveModel("gpt-b".to_owned()),
            "account/read and model/list are not sent again after the login"
        );

        let mut failed = ConnectionPreparation::default();
        failed.request(PreparationTrigger::OpenSettings);
        failed.begin();
        failed.finish_run();
        failed.record_login_reconciliation(context, true);
        failed.request(PreparationTrigger::LoginCompleted);
        failed.begin();
        assert_eq!(
            failed.next_step(&facts(context, ModelsFact::Failed, None), now),
            Step::Stop(PreparationFailure::ModelListFailed),
            "a failed catalog read is reported, not re-sent"
        );
    }

    #[test]
    fn failed_stage_does_not_advance_or_loop() {
        let now = Instant::now();
        let context = context(ActiveConnection::ChatGpt);
        let models = [model("gpt-a", true)];

        let mut prep = started(PreparationTrigger::OpenSettings);
        let mut unavailable = facts(context, ModelsFact::NotLoaded, None);
        unavailable.ready = false;
        assert_eq!(
            prep.next_step(&unavailable, now),
            Step::Stop(PreparationFailure::NotReady)
        );

        let mut account_failed = facts(context, ModelsFact::NotLoaded, None);
        account_failed.account_read_failed = true;
        account_failed.signed_in = false;
        assert_eq!(prep.next_step(&account_failed, now), Step::ReadAccount);
        prep.record_account_read(context);
        for _ in 0..3 {
            assert_eq!(
                prep.next_step(&account_failed, now),
                Step::Stop(PreparationFailure::AccountReadFailed)
            );
        }

        let mut signed_out = facts(context, ModelsFact::NotLoaded, None);
        signed_out.signed_in = false;
        assert_eq!(
            prep.next_step(&signed_out, now),
            Step::Stop(PreparationFailure::NeedsLogin)
        );

        let loaded = facts(context, ModelsFact::Loaded(&models), None);
        assert_eq!(prep.next_step(&loaded, now), Step::ReadModels);
        prep.record_models_read(context);
        let catalog_failed = facts(context, ModelsFact::Failed, None);
        for _ in 0..3 {
            assert_eq!(
                prep.next_step(&catalog_failed, now),
                Step::Stop(PreparationFailure::ModelListFailed)
            );
        }

        let missing = facts(context, ModelsFact::Loaded(&models), Some("removed"));
        assert_eq!(
            prep.next_step(&missing, now),
            Step::Stop(PreparationFailure::SavedModelMissing)
        );
        let empty = facts(context, ModelsFact::Loaded(&[]), None);
        assert_eq!(
            prep.next_step(&empty, now),
            Step::Stop(PreparationFailure::NoModelCandidates)
        );

        assert_eq!(
            prep.next_step(&loaded, now),
            Step::SaveModel("gpt-a".to_owned())
        );
        prep.record_model_saved();
        assert_eq!(
            prep.next_step(&loaded, now),
            Step::Stop(PreparationFailure::ModelSaveFailed),
            "a second automatic save in one run is refused"
        );
    }

    #[test]
    fn changed_context_rejects_old_stage_results() {
        let now = Instant::now();
        let old = context(ActiveConnection::ChatGpt);
        let mut changed = old;
        changed.settings_generation += 1;
        let mut renetworked = old;
        renetworked.network_revision += 1;
        let mut regenerated = old;
        regenerated.runtime_generation += 1;

        assert!(!old.same_configuration(&changed));
        assert!(!old.same_configuration(&renetworked));
        assert!(old.same_configuration(&regenerated));

        let models = [model("gpt-a", false)];
        let mut prep = started(PreparationTrigger::OpenSettings);
        read_everything(&mut prep, old);
        assert_eq!(
            prep.next_step(
                &facts(changed, ModelsFact::Loaded(&models), Some("gpt-a")),
                now
            ),
            Step::ReadAccount,
            "reads made under another configuration are not reused"
        );
    }

    #[test]
    fn successful_probe_is_reused_until_ttl_expires() {
        let t0 = Instant::now();
        let context = context(ActiveConnection::ChatGpt);
        let models = [model("gpt-a", true)];
        let facts = facts(context, ModelsFact::Loaded(&models), Some("gpt-a"));
        let key = ProbeKey::new(&context, 1, "gpt-a");

        let mut prep = started(PreparationTrigger::OpenSettings);
        read_everything(&mut prep, context);
        assert_eq!(prep.next_step(&facts, t0), Step::Probe(key.clone()));
        prep.record_probe(key.clone(), ProbeOutcome::Succeeded, t0);
        prep.finish_run();

        prep.request(PreparationTrigger::OpenSettings);
        prep.begin();
        read_everything(&mut prep, context);
        assert_eq!(
            prep.next_step(&facts, t0 + PROBE_SUCCESS_TTL - Duration::from_secs(1)),
            Step::Ready
        );
        assert_eq!(
            prep.next_step(&facts, t0 + PROBE_SUCCESS_TTL),
            Step::Probe(key),
            "an expired result is probed again, but only on a permitted trigger"
        );
    }

    #[test]
    fn failed_probe_is_not_retried_by_reopening() {
        let now = Instant::now();
        let context = context(ActiveConnection::ChatGpt);
        let models = [model("gpt-a", true)];
        let same_facts = facts(context, ModelsFact::Loaded(&models), Some("gpt-a"));
        let key = ProbeKey::new(&context, 1, "gpt-a");

        let mut prep = started(PreparationTrigger::OpenSettings);
        read_everything(&mut prep, context);
        prep.record_probe(key.clone(), ProbeOutcome::Failed, now);
        prep.finish_run();

        prep.request(PreparationTrigger::OpenSettings);
        prep.begin();
        read_everything(&mut prep, context);
        assert_eq!(
            prep.next_step(&same_facts, now),
            Step::Stop(PreparationFailure::ProbeFailed)
        );
        prep.finish_run();

        let mut regenerated = context;
        regenerated.runtime_generation += 1;
        prep.request(PreparationTrigger::OpenSettings);
        prep.begin();
        read_everything(&mut prep, regenerated);
        let regenerated_facts =
            facts(regenerated, ModelsFact::Loaded(&models), Some("gpt-a"));
        assert_eq!(
            prep.next_step(&regenerated_facts, now),
            Step::Stop(PreparationFailure::ProbeFailed),
            "runtime regeneration alone does not lift the suppression"
        );
    }

    #[test]
    fn explicit_retry_and_material_changes_rearm_preparation() {
        let now = Instant::now();
        let context = context(ActiveConnection::ChatGpt);
        let models = [model("gpt-a", true), model("gpt-b", false)];
        let key = ProbeKey::new(&context, 1, "gpt-a");

        let mut prep = started(PreparationTrigger::OpenSettings);
        read_everything(&mut prep, context);
        prep.record_probe(key.clone(), ProbeOutcome::Failed, now);
        prep.finish_run();

        // A different model is a material change.
        prep.request(PreparationTrigger::OpenSettings);
        prep.begin();
        read_everything(&mut prep, context);
        assert_eq!(
            prep.next_step(
                &facts(context, ModelsFact::Loaded(&models), Some("gpt-b")),
                now
            ),
            Step::Probe(ProbeKey::new(&context, 1, "gpt-b"))
        );
        prep.finish_run();

        // A new network revision is a material change.
        let mut renetworked = context;
        renetworked.network_revision = 1;
        prep.request(PreparationTrigger::OpenSettings);
        prep.begin();
        read_everything(&mut prep, renetworked);
        assert_eq!(
            prep.next_step(
                &facts(renetworked, ModelsFact::Loaded(&models), Some("gpt-a")),
                now
            ),
            Step::Probe(ProbeKey::new(&renetworked, 1, "gpt-a"))
        );
        prep.finish_run();

        // An explicit retry lifts the suppression for the same condition.
        prep.request(PreparationTrigger::Retry);
        assert_eq!(prep.begin(), Some(PreparationTrigger::Retry));
        read_everything(&mut prep, context);
        assert_eq!(
            prep.next_step(
                &facts(context, ModelsFact::Loaded(&models), Some("gpt-a")),
                now
            ),
            Step::Probe(key)
        );
    }

    #[test]
    fn cancel_stops_following_stages_without_rollback() {
        // The machine keeps no stage state beyond the run: ending the run
        // leaves the recorded reads and any saved model untouched, and nothing
        // pending, so no following stage starts.
        let context = context(ActiveConnection::ChatGpt);
        let mut prep = started(PreparationTrigger::OpenSettings);
        read_everything(&mut prep, context);
        prep.record_model_saved();
        prep.finish_run();
        assert!(!prep.has_pending());
        assert_eq!(prep.begin(), None);
    }
}
