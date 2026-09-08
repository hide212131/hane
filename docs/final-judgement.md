# Final judgement and merge gate

The controller implementation is in `.github/workflows/final-judge.yml`. Live end-to-end verification is tracked in Issue #44; implementing this workflow alone does not close that issue.

## Evidence and roles

Claude implements, Codex reviews, and Copilot judges. The final judge receives exact-head CI/review/GUI evidence as data. It has no tools and receives no workflow write token. The trusted controller independently checks its JSON recommendation against deterministic rules.

Every authenticated terminal GUI outcome (`pass`, `fail`, `blocked`, including recovered interrupted runs) reaches final judgement. No-GUI PRs proceed only after current CI, review, and policy classification are ready. GUI artifacts must come from the production GUI workflow on main, for the exact PR/SHA/run attempt/control SHA. Experiment artifacts cannot satisfy this gate.

A decision is bound to a fingerprint of the current evidence. Changed head, GUI generation, labels, review state, or required rules invalidate it. Repeated deliveries cannot repeat a paid judgement for unchanged evidence. A lost pending judge becomes blocked after its run completes. New evidence permits a fresh decision; controller/inference failures never mean pass.

## Automatic merge

The repository owner opts a PR into automatic merging with the `agentic-auto-merge` label. The label grants no exception to the checks below:

- Open, non-draft, same-repository PR from the owner or repository implementation bots, targeting main.
- Both required platform CI checks and the complete CI run pass. Missing, skipped, failed, stale, or unexpected required checks block merging.
- Current review clean, or exact-head findings with a later Copilot continue-validation route.
- No unresolved review threads or active changes-requested reviews.
- Current GUI policy classification; an authenticated current-generation GUI pass when required.
- No workflow file changes, including renamed workflow files. These remain owner-merge-only.
- Copilot final decision `ready` and unchanged evidence immediately before the GitHub merge request.

The merge request includes the exact head SHA and uses squash merge. GitHub also enforces its repository rules. A final decision artifact records the merge commit when successful. Unrelated PRs without the opt-in label are not automatically merged.

## Fix feedback

`final-fix-bridge.yml` authenticates a `fix` decision artifact and rechecks its evidence before dispatching the existing Claude worker. The worker accepts the newest substantive pre-GUI or final routing decision, keeps its existing iteration/lease/manual-retry limits, and revalidates final evidence before paid execution and before publishing changes. GUI failures and the final reason are included in the implementation prompt. A newer head or GUI generation invalidates the old authorization.

After a fix is pushed, existing trusted CI, Codex review, and GUI classification dispatches evaluate the new SHA; GUI and final workflows then reconcile that new evidence. `blocked` does not automatically authorize an implementation retry. Workflow-changing fixes retain the owner handoff policy.

## Evidence retention and limitations

Final receipts are retained for 30 days; production GUI receipts for 14 days. An expired or missing artifact cannot authorize a merge. The GUI suite covers focused OS keyboard, IME, reopen and wheel smoke scenarios, not every editor interaction. Live success, failure/recovery and automatic-merge evidence must be recorded before declaring Issue #44 complete.
