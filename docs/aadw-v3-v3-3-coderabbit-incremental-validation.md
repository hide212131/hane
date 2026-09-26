# AADW v3 V3-3 CodeRabbit incremental review validation

Issue: #358  
Parent: #333

This document is intentionally small. It exists only to produce an auditable
two-commit review sequence without mixing validation-only changes into product
pull requests.

## Baseline

- Repository: `hide212131/hane`
- Baseline branch point: `5558f4b0bab06078b4508763bf7ff7b1f9a51385`
- Validation kind: CodeRabbit baseline full review followed by incremental review
- Product code changed: no
- Commander Policy / AGENTS changed: no
- CodeRabbit autofix, fix-ci, merge, or unrelated write action requested: no

The baseline full review must record the exact PR head, CodeRabbit actor and run
ID, reviewed base-to-head range, selected files, completion result, actionable
result, and unresolved-thread count.

## Incremental review

After the baseline full review has completed, add exactly one small docs-only
commit to this file and record its new exact head. Compare the baseline head to
the new head and record the commit/file delta before requesting review.

Invoke `@coderabbitai review` on the new head. Treat it as successful
incremental evidence only when the CodeRabbit actor accepts and completes the
request and the observed review range covers the post-baseline delta rather
than re-running a full base-to-head review.

Record separately from the baseline:

- baseline head and new head;
- trigger comment and CodeRabbit actor/run;
- reviewed commit range and selected files;
- actionable or no-actionable result;
- unresolved review-thread count at the new head.

If the observed run is a full review, or the reviewed delta/range cannot be
established, the incremental acceptance remains unknown.
