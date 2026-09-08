# Final judgement and merge gate

The controller implementation is in `.github/workflows/final-judge.yml`. Live end-to-end verification is tracked in Issue #44; implementing this workflow alone does not close that issue.

## Evidence and roles

Claude implements, Codex reviews, and Copilot judges. The final judge receives exact-head CI/review/GUI evidence as data. It has no tools and receives no workflow write token. The trusted controller independently checks its JSON recommendation against deterministic rules.

Every authenticated terminal GUI outcome (`pass`, `fail`, `blocked`, including recovered interrupted runs) reaches final judgement. No-GUI PRs proceed only after current CI, review, and policy classification are ready. GUI artifacts must come from the production GUI workflow on main, for the exact PR/SHA/run attempt/control SHA. Experiment artifacts cannot satisfy this gate.

GUI claim/report jobs and final judgement share a serialized state-writer queue, so a new GUI generation cannot replace its evidence during final judgement/merge.

The snapshot includes the judge procedure version (`copilot-final/3`), so correcting the invocation or decision policy does not reuse a terminal result from an older procedure. The CLI receives a nonempty allowlist containing no actual tool, plus shell/write/URL denial and disabled built-in MCPs. An empty CLI allowlist would mean unspecified, and `--deny-tool '*'` is unsupported. Invocation diagnostics are bounded and redact supplied credential values.

The prompt defines the evidence schema: `ci_ready` includes exact-head platform check runs and the complete CI workflow, while `statuses` is only a selected commit-status subset. The classifier's `hane/gui-requirement = failure` means GUI is required, not that GUI execution failed. The authenticated `gui_receipt.outcome` holds the execution result. These meanings do not change the deterministic gate or remove independent review blockers.

A decision is bound to a fingerprint of the current evidence. Changed head, required GUI generation, labels, review state, or required rules invalidate it. A no-GUI snapshot ignores old GUI receipts and statuses. Repeated deliveries reuse retained final decisions. If a completed final fix run loses its receipt, the controller permits one fresh paid judgement (two claims total per evidence key), using status history to enforce the bound. Further receipt loss becomes explicitly blocked and requires new evidence or owner intervention. It never reconstructs Copilot approval from a status. A lost pending judge becomes blocked after its run completes. New evidence permits a fresh decision; controller/inference failures never mean pass.

A refused merge response records its reason and ends as `blocked`, even when Copilot recommended `ready`. After an unavailable response, the controller re-reads the PR: a confirmed merge of the same head records its merge SHA and `merged`; an unconfirmed or different-head outcome remains `blocked`. It does not repeat the paid judgement. Fresh evidence allows another attempt.

## Automatic merge

The repository owner opts a PR into automatic merging with the `agentic-auto-merge` label. The label grants no exception to the checks below:

- Open, non-draft, same-repository PR from the owner or repository implementation bots, targeting main.
- Both required platform CI checks and the complete CI run pass. Missing, skipped, failed, stale, or unexpected required checks block merging.
- Current review clean, or exact-head findings with a later Copilot continue-validation route.
- No unresolved current (non-outdated) review threads or active changes-requested reviews. GitHub must positively report that the PR is mergeable; unknown/conflicted results block.
- Current GUI policy classification; an authenticated current-generation GUI pass when required.
- No workflow file changes, including renamed workflow files. These remain owner-merge-only.
- Copilot final decision `ready` and unchanged evidence immediately before the GitHub merge request.

The merge request includes the exact head SHA and uses squash merge. GitHub also enforces its repository rules. A final decision artifact records the merge commit when successful. Unrelated PRs without the opt-in label are not automatically merged.

## Fix feedback

`final-fix-bridge.yml` authenticates a `fix` decision artifact and rechecks its evidence before dispatching the existing Claude worker. The worker accepts the newest substantive pre-GUI or final routing decision, keeps its existing iteration/lease/manual-retry limits, and revalidates final evidence before paid execution and before publishing changes. GUI failures and the final reason are included in the implementation prompt. A newer head or GUI generation invalidates the old authorization.

After a fix is pushed, existing trusted CI, Codex review, and GUI classification dispatches evaluate the new SHA; GUI and final workflows then reconcile that new evidence. `blocked` does not automatically authorize an implementation retry. Workflow-changing fixes retain the owner handoff policy.

## Evidence retention and limitations

Final receipts are retained for 30 days; production GUI receipts for 14 days. An expired or missing artifact cannot authorize a merge. The GUI suite covers focused OS keyboard, IME, reopen and wheel smoke scenarios, not every editor interaction. Live success, failure/recovery and automatic-merge evidence must be recorded before declaring Issue #44 complete.

The owner-only `Final judge terminal-outcome probe` dispatch exercises the live Copilot call using a real complete GUI-pass snapshot and explicitly fault-injected fail/blocked copies. It has read-only GitHub permissions and publishes no PR statuses, dispatches, or merges. Its artifacts label the injected cases; they are boundary-test evidence, not claims of real application failures and never production GUI receipts.
