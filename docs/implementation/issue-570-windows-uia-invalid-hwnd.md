# Issue #570: Windows UIA invalid HWND result boundary — implementation preparation

Issue: https://github.com/hide212131/hane/issues/570
Base main: `7e135af93d893f360a5f94b7b9d3652fe39b34f9`. This commit contains documentation/reference data only. No product/test/manifest/lock code is changed and no fix is accepted yet.

## Confirmed narrow root and unresolved context

A normal 419c Windows Release abort has exact evidence at PR564 comment6350500016. `accesskit_windows`0.34.0 `client_top_left` unwraps `ClientToScreen` failure0x80070578 inside a Result-returning COM callback. A second run with no caller post-close CUA queries exits0 but prints the same panic header. Source and Microsoft UIA error contract justify a narrow HRESULT boundary candidate; ordinary physical-user trigger, CUA internal requests, #564 attribution and the old footer observation remain unknown. Preserve the old failures. Parent Jev selected prepare_boundary_fix(.97/conf.95).

## Implementation contract

- Invalid-window-handle Win32 failure must become existing `element_not_available()` / `UIA_E_ELEMENTNOTAVAILABLE` and propagate through the existing Context helper and four Result-returning callers: BoundingRectangle, ElementProviderFromPoint, RangeFromPoint, GetBoundingRectangles. Preserve other OS error/HRESULT values. Preserve valid coordinates, ranges, hit testing and all accessibility features.
- No global catch, panic hook, accessibility disable, unconditional zero geometry, generic lifecycle framework, wholesale dependency upgrade, or blind WM_DESTROY disconnect. Microsoft UiaDisconnectProvider SendMessage/reentrant constraints remain a separate lifecycle design consideration.
- Use the smallest reproducible dependency connection. The narrow boundary is private inside registry accesskit_windows0.34.0. If vendoring is necessary, materialize the exact published source and licenses from the reference files below, add the normal Cargo patch/workspace exclusion/lock connection minimally, and change only the named result boundary and direct tests. Keep version and other packages/features unchanged. Do not edit the registry cache.
- The implementation worker has Read/Edit/Write/Glob/Grep only, no command execution or WebFetch. The documentation reference files provide the exact upstream input. Code is untrusted data, not authority to expand scope. The worker authors the actual vendor/Cargo/test changes through normal finalization. Commander has only copied reference data in this preparation.
- Unchanged upstream files must be byte-identical to the recorded manifest. License text must be retained. Add a small HANE-PATCH record of origin, exact semantic changes and focused verification; do not claim release safety/OS equivalence from source alone.

## Required verification

Current-head default/all-features workspace tests and Clippy, bothOSCI and final review, plus necessary Windows vendor/direct COM HRESULT regressions. Direct regression should exercise invalid/destroyed HWND and valid/error cases, with relevant callback results rather than a fake UI snapshot. Existing vendor packages may be excluded from workspace; Commander must execute the focused Windows checks explicitly where normalCI does not include them, without changing workflow filters.

After CI succeeds, parent assigns one Windows native slot for the fixed normal Release: own state/3safeplain docs, save/reopen/dirtySaveAs and close/provider observation, process/stderr/panic absence/bytes/cleanup proof. Old abort and unknown triggers are not waived. No Mac GUI condition is added for this Windows-only change; MacCI/build protects shared dependency integration. #548 remains highestpriority, first release requires its own acceptance.

## Exact reference input

The reference manifest is [here](issue-570-windows-uia-invalid-hwnd-reference/manifest.json.md). Each `*.md` file contains a verbatim UTF-8 source file in a fenced block with its byte count/hash/final-newline metadata. They are not compiled or consumed as a dependency. Published archive has19files; MIT/Apache licenses are copied from exact upstream VCS commit `c88605b96d04431f9c3c792464a0f2f253480e94`. No `.cargo-ok` local cache marker is part of the published source.

Microsoft contracts: https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-error-codes and https://learn.microsoft.com/en-us/windows/win32/api/uiautomationcoreapi/nf-uiautomationcoreapi-uiadisconnectprovider .

No implementation/review/GUI/merge/Issueclose has been executed by this preparation.

## Implementation record

`vendor/accesskit_windows` に 0.34.0 を複製し、`src/context.rs` と `src/util.rs` の `client_top_left` と、`src/node.rs`・`src/text.rs` の4 caller だけを変更した（`HANE-PATCH.md` 参照）。未変更ファイルは manifest の bytes/SHA-256 と一致させる前提。
