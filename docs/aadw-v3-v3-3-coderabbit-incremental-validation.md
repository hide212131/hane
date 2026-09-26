# AADW v3 V3-3 CodeRabbit incremental review validation

Tracking: #360 / parent #333

This document exists only to provide a minimal, reviewable delta for the V3-3 incremental-review connection test.

## Baseline

The first commit establishes a docs-only baseline. A CodeRabbit full review must complete on this exact head before a second commit is added.

## Boundaries

- This validation does not change product code, workflows, Commander Policy, or agent instructions.
- CodeRabbit is used as a reviewer only. Autofix, CI fixes, merge-conflict fixes, generated tests, and merge delegation are outside this validation.
- GUI validation is not required for this docs-only validation PR. Existing Hosted GUI route evidence is tracked separately in #346.
- A later `@coderabbitai review` counts only if CodeRabbit reports an incremental range after the baseline review.

## Incremental probe

After the baseline full review completed on `b96761c7aad58487a8bf8729a2d32eaaebf97267`, this section was added as the only second-commit delta. The next review request must be `@coderabbitai review`, and the evidence must show that the reviewed commit range starts from the already-reviewed baseline rather than from the PR base.

## Observed incremental review evidence

The manual incremental request was posted by `hide212131` in PR comment `5845542029` after the baseline full review had finished.

- CodeRabbit actor: `coderabbitai[bot]`
- command response: PR comment `5845544615`, `Review finished.`
- incremental Run ID: `10fb0a5f-4a26-4b52-b6d0-b5c0160d028b`
- baseline reviewed head: `b96761c7aad58487a8bf8729a2d32eaaebf97267`
- new reviewed head: `1dd26987a9186b1ad2555ee16011d49b9f3dee9a`
- reviewed range: `b96761c7aad58487a8bf8729a2d32eaaebf97267..1dd26987a9186b1ad2555ee16011d49b9f3dee9a`
- selected file: `docs/aadw-v3-v3-3-coderabbit-incremental-validation.md`
- excluded changed files: none; the second commit changed only the selected file
- result: completed with 1 actionable documentation finding (review `5325641502`, thread `4111120286`)
- unresolved threads immediately after the incremental review: 1

The actionable finding required this request/run/range/result association to be recorded. This evidence therefore demonstrates an actual incremental review, not a second full review. The finding is addressed by this evidence-only documentation update; a final current-head full review is still required before merge.
