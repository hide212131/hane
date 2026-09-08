# Claude retry state and acceptance criteria

This change is separate from PR #82's notification permission fix. It does not
change `pull-requests` permissions. The old expanded PR head is preserved at
`preserve/pr82-5e50365` (`5e50365815fcc9f6c525d4bd4d73a2ccd453cdb9`).

## Contract

A manual authorization is identified by the owner comment ID and the current
head SHA. Its status context is `hane/claude-fix-manual/<comment-id>-1`.
Re-running the routing workflow does not create or renew an authorization.

| State | Status | Allowed next action |
| --- | --- | --- |
| Unrecorded | No context | An owner command may record authorization |
| Authorized | `success`, exact `authorized for <sha>` description | Dispatch/recover delivery; worker may claim once |
| Claimed | `pending`, exact `claimed for <sha>` description | Invoke Claude once, or await owner if execution is uncertain |
| Consumed | `error`, exact `consumed for <sha>` description | No replay; only a new owner command permits another invocation |
| Pushed | New bot commit with matching parent and manual trailer | New head starts a fresh automatic cycle; old-head deliveries are stale |

Worker executions are serialized per PR. The reconciler only redispatches;
it never claims or renews authorizations. Before invoking Claude, the worker
revalidates the specific context and writes its claim. If the write fails,
it does not invoke Claude, even if GitHub may have persisted the claim.
This chooses **at-most-once paid invocation per manual authorization**, rather
than promising exactly-once execution across a process crash. A crash between
the claim and invocation can require another explicit command. A claim that
has timed out is not automatically returned to authorized.

Reduce latest status **within each context** before selecting an outstanding
authorization. Completion of A cannot hide B. A previous failed/no-change/
workflow-handoff result does not veto a new unclaimed command. Existing live
execution leases and controller backoff are respected before redispatch.

The worker and reconciler use `.github/scripts/claude_fix_state.py` for these
decisions. They copy it from `github.workflow_sha` before checking out the PR,
so the controller and policy come from the same workflow revision. Pull request
code is not imported to make the pre-execution authorization decision.

The latest routing event is also authoritative in both worker and recovery.
Controller-failure notifications may be skipped while reading history, but a
newer `continue-validation`, `blocked`, pending, unknown or malformed decision
must never revive an older fix. An owner grant bypasses only an owner-only
workflow guard; it does not override a newer no-fix decision. The worker checks
this shared policy both during resolution and after claiming, before its final
PR eligibility lookup. This covers [discussion 3959897165](https://github.com/hide212131/hane/pull/85#discussion_r3959897165)
with routing regressions and a real claim-shell simulation of a newer no-fix
arriving during the claim POST.

## Cycle budget

The budget limits successful automatic fixes, not total workflow runs. Each
completion belongs to the target commit of that fix. A successful manual fix
creates boundary M; the first automatic fix targets M and **does** count.
The manual fix targeting M's parent does not count in M's cycle.

The worker obtains every status page and the PR commit list. A matching bot
fix commit and its actual parent are durable evidence of completion even if
completion/reset status writes were lost. Completion statuses and commit
evidence are deduplicated by target SHA. Boundary calculation uses commit
order, not status ID or publication timestamp. The reconciler can still repair
status markers for visibility, but the worker does not wait for that repair
to enforce the budget correctly.

## PR #82 finding disposition

All links refer to the original review. “Covered” describes code and local
regression coverage; it is not a claim of a new reviewer approval.

| Original discussion | Disposition in this split |
| --- | --- |
| [3951377867](https://github.com/hide212131/hane/pull/82#discussion_r3951377867) permission/403 | Remains in notification PR #82; no permission change here |
| [3951493127](https://github.com/hide212131/hane/pull/82#discussion_r3951493127) manual completion consumes slot | Covered: `BudgetTests.test_manual_fix_does_not_consume_any_of_three_new_slots` |
| [3953155700](https://github.com/hide212131/hane/pull/82#discussion_r3953155700) partial finalization loses reset | Covered: `BudgetTests.test_missing_both_completion_and_reset_posts_still_counts_pushes` |
| [3953209507](https://github.com/hide212131/hane/pull/82#discussion_r3953209507) reconciled status ordering | Covered: `BudgetTests.test_delayed_statuses_do_not_change_budget_or_double_count` |
| [3953209510](https://github.com/hide212131/hane/pull/82#discussion_r3953209510) reset POST unrecoverable | Durable commit boundary, independent of either reset POST |
| [3958763624](https://github.com/hide212131/hane/pull/82#discussion_r3958763624) boundary off-by-one | Budget tests exercise 0 through 4 automatic fixes after manual boundary |
| [3958763633](https://github.com/hide212131/hane/pull/82#discussion_r3958763633) block before asynchronous recovery | Budget computed from pushed commits before limit enforcement |
| [3958824740](https://github.com/hide212131/hane/pull/82#discussion_r3958824740) boundary lost at next head | Budget scans all PR commits; tests include multiple later heads |
| [3958892141](https://github.com/hide212131/hane/pull/82#discussion_r3958892141) manual authorization lost on recovery | `RecoveryTests.test_delivery_failure_before_claim_recovers_same_authorization` |
| [3959089069](https://github.com/hide212131/hane/pull/82#discussion_r3959089069) blocked state hides new authorization | `RecoveryTests.test_new_command_recovers_after_every_old_terminal_outcome` |
| [3959089075](https://github.com/hide212131/hane/pull/82#discussion_r3959089075) unvalidated manual trailer | Validate bot, exact generated subject and actual parent; negative budget tests |
| [3959154318](https://github.com/hide212131/hane/pull/82#discussion_r3959154318) paid terminal retry loop | Durable pre-execution claim; terminal/lost-POST recovery tests |
| [3959229674](https://github.com/hide212131/hane/pull/82#discussion_r3959229674) alternate automatic dispatchers bypass guard | Retained central changed-workflow guard in worker, before any paid invocation |
| [3959286302](https://github.com/hide212131/hane/pull/82#discussion_r3959286302) guard blocks explicit owner retry | Retained manual exception after validating authorization |
| [3959341984](https://github.com/hide212131/hane/pull/82#discussion_r3959341984) controller status hides fix | `RecoveryTests.test_controller_failure_preserves_earlier_fix_but_not_owner_only_route` |
| [3959341995](https://github.com/hide212131/hane/pull/82#discussion_r3959341995) old authorization/queued dispatch replay | Per-context latest state, revalidation, pre-execution claim; shell duplicate-delivery test |
| [3959418598](https://github.com/hide212131/hane/pull/82#discussion_r3959418598) owner-only route blocks recovery | `RecoveryTests.test_workflow_owner_route_requires_active_manual_authorization` |
| [3959418607](https://github.com/hide212131/hane/pull/82#discussion_r3959418607) A consumes B | Separate contexts; exact production terminal shell tests for failure, no changes and patch handoff |
| [3959487382](https://github.com/hide212131/hane/pull/82#discussion_r3959487382) latest global status hides B after A completes | `AuthorizationTests.test_old_consumption_never_masks_new_grant_regardless_of_api_order`; all status order permutations retain B |

## Validation and review completion

Run `python3 -m unittest discover -s .github/tests -p 'test_*.py'`.
The normal CI workflow runs these same tests without tokens for write operations,
network calls from the tests, or Claude/Copilot invocation. The shell fixtures
replace `gh` and execute the actual claim, manual-dispatch and terminal steps.
They exercise successful writes, ambiguous persisted-but-failed writes,
dispatch failures and duplicate delivery.

Local validation: 79 tests passed, including 40 workflow-state regressions; YAML parsing
and Bash syntax passed for all changed workflows. Actionlint 1.7.12 passed
with only its unsupported existing `concurrency.queue` syntax excluded. That
syntax is documented by [GitHub](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax).
No live paid retry, notification POST, or main-branch rollout was performed.

The pre-invocation guard re-reads PR eligibility after the claim write. A head
move during setup or claim publication, a closed/Draft PR, or a failed final
read prevents invocation. The claim is not renewed on these paths. This covers
[PR #85 discussion 3959823708](https://github.com/hide212131/hane/pull/85#discussion_r3959823708)
with four production-shell regressions (including both automatic and manual delivery).

A clean pre-invocation abort publishes `Claude fix not started` to release the
execution lease. A subsequent authoritative fix (automatic case) or new owner
grant can proceed immediately instead of waiting 55 minutes for a process that
never started. The authorization claim itself is not renewed. This covers
[discussion 3959981424](https://github.com/hide212131/hane/pull/85#discussion_r3959981424).

Trusted-generation finalization depends on the workflow-state test job as well
as platform report jobs. A failing state test publishes a failed generation;
cancelled/skipped state tests yield an incomplete generation. Successful state
tests cannot mask a platform failure. The production finalizer shell is tested
for these cases, covering [discussion 3959981430](https://github.com/hide212131/hane/pull/85#discussion_r3959981430).

Before leaving draft:

1. Review the explicit at-most-once contract and its crash tradeoff above.
2. Ensure the workflow state regression CI job passes for the final head.
3. Review remaining findings against the named scenario/contract, recording
   an implementation regression or a separately proposed requirement.
4. Roll out through the normal reviewed merge path. `repository_dispatch` and
   scheduled recovery still run main's definitions until merge; a successful
   run against main is not evidence that this branch's controller ran.
5. Confirm the first owner-approved production attempt by its target SHA,
   authorization context, run URL and terminal result. Do not automatically
   replay an uncertain claim.
