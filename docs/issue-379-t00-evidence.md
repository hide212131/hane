# Issue #379 T00: standalone safety-profile evidence

Date: 2026-09-28 (JST)
Repository base at run: `db7c5f4121be810addbd509a60415d2833284bf8`
Target: Codex App Server standalone `0.157.1`, macOS Apple Silicon

## Binary and invocation

The official `openai/codex` release `rust-v0.157.1` asset
`codex-app-server-aarch64-apple-darwin.tar.gz` was used. The archive SHA-256
was `a427487e775e3053feacb9dee699439e36e1dac2fe9516fa5c22e922fcc70d60`;
the extracted executable reported `codex-app-server 0.157.1`.

The process was the standalone executable, started directly with:

```text
codex-app-server-aarch64-apple-darwin --strict-config --listen stdio:// --session-source mcp
```

No full-CLI `app-server` subcommand was used. The config was written to a new
per-run `CODEX_HOME`, and the real user `HOME` was retained. The probe cwd was
a new empty test workspace. Its parent contained sentinel `AGENTS.md`,
`.codex/config.toml`, and neighbor skill files. A deliberately unrelated
`OPENAI_API_KEY` / `OPENAI_BASE_URL` pair was also present in the child
environment. These were disposable test values.

## Observations

| Check | Result |
| --- | --- |
| Strict-config startup and fixed mock response | Passed; the mock Responses server received `POST /v1/responses` and returned `P0-OK`. |
| Selected Custom destination, model, and key | Passed for this fixture: the request used the configured loopback endpoint, model `gpt-p0-fixed`, and the expected dummy Bearer key. |
| Fixed input and nearby sentinels | The provider request contained the fixed user text only; the sentinel string was absent. `thread/start` returned an empty `instructionSources` list. |
| No tools in the provider request | **Failed.** The request contained five tools; names included `request_user_input`, `get_goal`, `create_goal`, and `update_goal`, plus one unnamed tool entry. All feature toggles in the fixture were false, including `shell_tool`, `apply_patch`-related flags, MCP/apps/plugins/skills, search, image, browser, computer use, and multi-agent flags. |
| Unexpected returned function call | A negative response attempted `exec_command` to create a marker in the disposable temp directory. The standalone logged `unsupported call: exec_command`; no marker appeared. This only establishes rejection of that specific call and does not compensate for the non-empty exposed tool list. |
| External developer context | **Failed.** The request still contained a developer input block even with the clean child environment, an empty developer-instructions config value, `include_environment_context = false`, and `skip_host_skill_discovery = true` in the generated feature settings. The initial inherited-environment run showed host skill/tool instructions in this block; in the allowlisted run the block's presence was recorded but its content was not retained. The nearby sentinel content was absent. The source of the residual developer block is unresolved. |

The same five tool names and developer-context presence were observed both
with the original inherited environment and with a minimal allowlisted child
environment. The allowed environment retained `PATH`, the real `HOME`, user,
temporary-directory, locale, certificate, and proxy variables, plus the
explicit `CODEX_HOME` and Custom key. It did not contain
`CODEX_APP_TOOLS_PIPE_PATH`, `CODEX_SESSION_ID`, `CODEX_THREAD_ID`,
`CODEX_PERMISSION_PROFILE`, or `CODEX_MCP_NODE_PATH`.

## T00 decision

**T00 fails; the safety profile is not proven.** The mock success proves only
that the selected Custom endpoint receives the fixture request. Tool
definitions and host developer context remain in the provider request. The
negative function-call case is not evidence that all tools are disabled.
The fixed-response connection-confirmation feature must remain unpublished
until a versioned profile and negative tests demonstrate an empty tool list,
no external context, and no execution for unexpected tool responses.

This run was a Custom mock test only. It is not ChatGPT OAuth evidence, a
Windows run, or GUI evidence. Full request bodies, host instructions,
credentials, and raw authorization values were not retained in this file.

## Commander acceptance and re-evaluation

On 2026-09-28, the Commander explicitly accepted the presence of provider
tool definitions and the additional developer input block as product behavior
that is allowed and not itself a problem. This supersedes the original T00
failure classification for those two observations; the observations above
remain unchanged. The design PR was not modified.

Under that accepted criterion, this recorded fixture passes: the configured
Custom endpoint/model/key were used, the fixed user input was sent, the nearby
sentinel was absent from the provider request, and the unexpected
`exec_command` function call was rejected as unsupported with no marker file
created. `thread/start` also returned no instruction sources. This is a scoped
result for the tested `exec_command` response; it does not claim that the tool
list was empty or prove the behavior of every possible tool call. The
Commander accepted the presence of the tool definitions and developer input
block, while this evidence retains only the negative-call behavior actually
observed.

## 2026-09-29 standalone retest and isolation-profile fix

This section records the follow-up assigned for P0/T00. It supersedes the
earlier inference that absence of a sentinel from a provider request alone
proved it had not been read. A baseline run with Codex's default nonzero
`project_doc_max_bytes` did include the workspace `AGENTS.md` sentinel in the
provider input. The generated Hane profile now sets the three explicit
0.157.1 isolation controls listed below; the UI Probe gate remains disabled.

### Pinned binary and generated profile

- Platform: macOS 26.6.2, Apple Silicon.
- Official release: `openai/codex` tag `rust-v0.157.1`, peeled source commit
  `36650394c5b38c2990ccf2a3457165ca3e9d9726`.
- Asset: `codex-app-server-aarch64-apple-darwin.tar.gz`, SHA-256
  `a427487e775e3053feacb9dee699439e36e1dac2fe9516fa5c22e922fcc70d60`.
- Extracted executable SHA-256:
  `0e600652a21c97675a92b8410e350942e8bcd6f574788df5c8de2c0b909d549c`;
  it reports `codex-app-server 0.157.1`.
- Official config schema asset SHA-256:
  `17fbda7e71603aa6a83e986608f2e13c27ea46ce1a4b889196ba81a07c39d907`.
- The executable was run directly as the standalone binary with
  `--strict-config --listen stdio:// --session-source mcp`. The full CLI's
  `app-server` subcommand was not used.
- `CODEX_HOME` was a fresh disposable directory, with a dummy Custom key and
  loopback-only mock Responses provider. Real host plumbing variables for
  app tools/session/permissions/MCP were absent from the child environment.
  A deliberately unrelated `OPENAI_API_KEY` and `OPENAI_BASE_URL` pointed to a
  separate loopback trap, to check that the selected Custom config controlled
  destination and model.
- The generated ChatGPT config and Custom config now both set
  `project_doc_max_bytes = 0`, `project_root_markers = []`, and
  `[features] skip_host_skill_discovery = true`. The Custom config retains its
  configured provider/model. Unit tests parse both generated configs and
  assert these values; `--strict-config` accepted the same profile in the
  standalone run.

### Reproduction and observations

The sanitized harness and result summary were kept locally at
`/tmp/hane379-t00-20260929/run_t00.py` and
`/tmp/hane379-t00-20260929/t00-run-summary.json`. It creates disposable
fixtures for a regular-file case, a FIFO tripwire case, and a negative tool
case:

1. Regular nearby-file case: the working directory contains an
   `AGENTS.md` sentinel. Its parent contains a `.git` directory, a `.codex`
   project config with a distinct developer sentinel, an invalid unknown
   config key and a conflicting model/provider routed to the trap, and
   sentinel skill files under both `.codex/skills` and `.agents/skills`.
   The Responses mock records only request path/host/model, tool names/count,
   boolean checks for the fixed input and sentinel strings, and whether its
   dummy authorization matched. It never stores request bodies or auth
   values.
2. Read-tripwire case: those same external `AGENTS.md`, parent
   `.codex/config.toml`, `.codex/skills/.../SKILL.md`, and
   `.agents/skills/.../SKILL.md` locations are FIFO files with no writer.
   An open/read path that waits for their contents would prevent the server
   from completing initialization or the fixed turn.

For each case, the harness sends JSON-RPC `initialize`, `thread/start`, and
`turn/start` to the standalone App Server and a fixed user input to the mock
Responses endpoint. In a third negative case, the mock instead returns an
unadvertised `exec_command` function call whose proposed command would create
a marker and POST to the separate trap endpoint.

| Check | Result |
| --- | --- |
| Strict standalone startup and fixed response | **Pass** in regular-file and FIFO cases; both turns completed without a turn error. |
| Destination, model, input, dummy credential | **Pass**; requests went to the configured loopback host at `/v1/responses`, model `gpt-p0-fixed`, with the fixed input and matching dummy bearer value. The unrelated OpenAI URL/key was not selected. |
| Nearby sentinel contents | **Pass** for this profile; request input had none of the AGENTS/config/skill sentinel strings. |
| External file read tripwires | **Pass, scoped**; initialization and the turn completed while the external config, AGENTS file, and both adjacent skill files were FIFOs with no writer. The regular-file case also showed no trap request or external provider/model selection. |
| Unexpected `exec_command` response | **Pass for this named call**; server log indicated `unsupported call: exec_command`; the marker was absent; the separate trap recorded no paths. The turn completed and the server stopped with exit code 0. |
| Other provider-visible tools | One `request_user_input` definition remained in each provider request. Commander has explicitly accepted provider tool definitions and extra developer input as product behavior; this result does not claim an empty tool list or prove every possible tool name/call. |
| Child stop | **Pass**; all three tested App Server processes stopped and returned exit code 0 after the harness closed their stdio input. |

### Pinned-source contract and limits

The release schema permits `project_doc_max_bytes = 0` (minimum 0; default
32768) and an empty `project_root_markers` array. In the pinned source at
commit `36650394c5b38c2990ccf2a3457165ca3e9d9726`,
`codex-rs/core/src/agents_md.rs` breaks before calling `read_agents_md` when
the remaining instruction budget is zero. In
`codex-rs/config/src/loader/mod.rs`, empty project-root markers return before
walking cwd ancestors; root resolution then uses the current runtime cwd. The
per-run cwd is a newly created empty Hane workspace. In
`codex-rs/core/src/session/session.rs`, `skip_host_skill_discovery` returns
before host skill discovery when no registered extension requires it; the
fixture had no such extension. These contracts match the regular-file and
FIFO observations.

No kernel-level open trace was available: macOS `fs_usage` refused to run
without root, and noninteractive sudo was unavailable. Therefore this record
does not claim an OS syscall trace. The no-read finding is scoped to these
Codex 0.157.1 paths and files, based on the pinned early-return contracts and
the unopenable FIFO tripwires, plus the regular-file sentinel/routing checks.
It does not establish behavior for every Codex version, arbitrary extensions,
or every possible returned tool name. The app-server protocol did not provide
`instructionSources` in these retest responses, so the new run does not claim
an empty list.

### Local code validation

- `cargo test -p hane-ai`: **pass** — 134 unit, 11 account-service, 16 Custom
  Provider, and 20 runtime-lifecycle tests. Output:
  `/tmp/hane379-t00-20260929/cargo-test-hane-ai.log`.
- `cargo fmt --all -- --check`: **fail** — rustfmt reports repository-wide
  formatting differences in existing code, including older sections of
  `provider.rs`; it was not applied wholesale to avoid unrelated changes.
  The new code blocks in `provider.rs` are formatted, and a focused
  `rustfmt --edition 2024 --check crates/ai/src/connect.rs` passed.
- `git diff --check`: **pass** for this worktree.

### T00 result

**Pass for the tested Codex 0.157.1 standalone profile and the scoped cases
above.** The finding that default configuration loaded the AGENTS sentinel led
to the isolation-profile change; both generated Hane provider profiles now
set the controls that prevent that path. The negative execution check remains
limited to the unadvertised `exec_command` call. On 2026-09-29 the Commander
accepted this scoped result and assigned P5. Commit `21a64d5393cab4ee1709b7bfbd3f2d47e1f5f412`
enables the fixed-response Probe gate on that basis; enabling it does not
constitute P5 OAuth, Custom Provider, or native GUI evidence.

### P5 status at head `21a64d5393cab4ee1709b7bfbd3f2d47e1f5f412`

- Current implementation PR #397 remains Draft/Open on
  `feat/379-chatgpt-codex-oauth-ai-settings`; Issue #379 remains open. The
  design-only PR #392 remains open at
  `025c53d0d03a9ecc2a221d80257d88e59b6c5dfd`.
- `cargo test -p hane-ai`: **pass** (134 unit, 11 account-service, 16 Custom
  Provider runtime, 20 runtime-lifecycle tests); log:
  `/tmp/hane379-p5-20260929/cargo-test-hane-ai.log`.
- `cargo test -p hane-ui view::ai_settings::tests`: **pass** (3 tests); log:
  `/tmp/hane379-p5-20260929/cargo-test-hane-ui-ai-settings.log`.
- `rustfmt --edition 2024 --check crates/ui/src/view/ai_settings.rs`:
  **pass**; log `/tmp/hane379-p5-20260929/rustfmt-ai-settings.log`.
- `cargo build -p hane`: **pass**; log
  `/tmp/hane379-p5-20260929/cargo-build-hane.log`; built binary SHA-256
  `91496f4f706926a70107608c848cee9deca8a0c2d8557af34dc342a77b255873`.
- `cargo fmt --all -- --check`: **fail** due existing repository-wide
  rustfmt differences; the AI settings file itself passes the focused check.
  Log `/tmp/hane379-p5-20260929/cargo-fmt-all-check.log`.
- GitHub Actions run `36518580844` at this exact head: **pass** for macOS and
  Windows workspace tests and Clippy, including macOS-only fallback glyph and
  input-source tests. This CI result is not native GUI evidence.
- T29 actual ChatGPT OAuth is **not accepted as pass**. A first temporary UI
  attempt was not counted because state isolation was not established, and the
  visible account state remained signed out. A new app copy with
  `HANE_STATE_DIR=/tmp/hane379-p5-20260929/app-state-clean` and a disposable
  workspace was launched, but it remained at “Opening work folder…” and the AI
  settings view was not reached. No successful sign-in, fixed response,
  logout, or re-login was demonstrated. No credential was copied into test
  logs or the evidence file.
- T30 real Custom Provider connection is **not run**. The provider Base URL,
  model ID, and direct UI key entry are still required. The passing mock
  runtime tests are not evidence of a real provider connection.
- T31 is **partial**: exact-head CI passed; macOS native GUI is blocked at the
  steps above; no Windows device was selected; CodeRabbit did not review this
  Draft PR (the status context is not a review). GitHub reported no submitted
  PR reviews. The PR remains Draft; no review trigger, readiness change,
  merge, or Issue close was performed.

### P5 GUI reproduction and remaining inputs

For the macOS reproduction, build `hane` at the recorded head, copy it into a
temporary `.app` with bundle ID `io.github.hide212131.hane.p5isolated`, set
`LSEnvironment.HANE_STATE_DIR` to a new mode-0700 directory under
`/tmp/hane379-p5-20260929`, and point its isolated `settings.conf` default
folder at a one-file temporary Markdown workspace. On this run, the app
remained at “Opening work folder…” for more than three minutes; the window can
be observed, but CUA click actions return `noWindowsAvailable`, so Settings → AI
cannot be exercised through the current automation session. The app’s state
root and workspace are separate from Hane’s normal user state. The user must
navigate the isolated app to Settings → AI before the OAuth handoff can resume.

Remaining Commander inputs for P5 are the selected Windows GUI device and the
Custom Provider Base URL/model ID. The API key must be entered directly into
the isolated Hane UI; it must not be sent in chat or written to logs.

### Verification refresh at documentation head `ac4d8ff813c71a47e2f433ab5f9440454a3b61ff`

This commit changes only this evidence file; product code is unchanged from
the tested gate commit `21a64d5393cab4ee1709b7bfbd3f2d47e1f5f412`.

- `cargo test -p hane-ai`: **pass** (134 unit, 11 account-service, 16 Custom
  Provider runtime, 20 runtime-lifecycle); log:
  `/tmp/hane379-p5-20260929/cargo-test-hane-ai-ac4d8ff.log`.
- `cargo fmt --all -- --check`: **fail** on the same existing repository-wide
  formatting differences, including `atomic_file.rs`, `credential_journal.rs`,
  `owner_lock.rs`, `protocol.rs`, and `provider.rs`; log:
  `/tmp/hane379-p5-20260929/cargo-fmt-all-check-ac4d8ff.log`.
- GitHub Actions run `36521157048` at this head: **pass** for macOS and Windows
  workspace tests and Clippy, including both macOS-specific tests.

### UI follow-up and latest P5 evidence at head `4f3d4113b3d5e2939c840e5efbce38f822251e27`

- The implementation branch started this follow-up at `deee72a98c8229846a351e3470d1cf71976cbaa1`; only `crates/ui/src/view/ai_settings.rs` changed. Current `main` is `5be310c6136e662119e2d208322f832b317e14c4`, while PR #397 still reports base `864802015ba943f4c68108a6c2cb88d557195a07`. The intervening `main` commits only changed GUI-validation workflows/scripts/docs, with no overlapping product files; this candidate was kept without rebasing.
- T00 remains **pass within the recorded 0.157.1 standalone profile and scoped cases**. The evidence and reproducible commands remain in this file's earlier T00 section; this UI-only commit does not change the tested safety profile.
- T01–T23 automated cases were rerun at this source state through `cargo test -p hane-ai`: **181 passed** (134 unit, 11 account-service, 16 Custom Provider runtime, 20 runtime-lifecycle). The per-ID scope and remaining partial acceptance gaps remain as listed in PR #397's T00–T31 matrix. Log: `/tmp/hane379-p5-20260929/cargo-test-hane-ai-final-precommit.log`.
- T24 remains **partial**. Added `view::ai_settings::tests::chatgpt_model_selection_can_be_changed_multiple_times_before_saving` and `view::ai_settings::tests::fixed_probe_button_submits_a_probe_command`; both pass within `cargo test -p hane-ui` (**218 passed**). The first test selects two distinct model buttons in one unsaved draft. The second confirms the UI submits `AiCommand::Probe`; it uses an unconfigured service and is not a real provider-response test. Log: `/tmp/hane379-p5-20260929/cargo-test-hane-ui-final-precommit.log`.
- T25–T28 retain their previous matrix outcomes. The full UI suite was rerun at this source state (**218 passed**); native Japanese IME/Tab and Windows GUI checks remain outstanding. T26/T27 automated evidence includes `external_open_does_not_close_an_open_settings_page` and `ai_secret_input_is_masked_and_editor_shortcuts_do_not_reach_document`.
- T29 is **reported complete by the user**. Prior direct app observation showed real ChatGPT OAuth signed in, seven models, GPT-6-Luna saved, and fixed Probe success (`HANE_AI_OK`); the user subsequently confirmed logout/re-login and model refresh. The later supplied screenshot is after switching to Custom Provider and shows the ChatGPT account as `未確認`; it records the current screen, not the earlier OAuth result. No account identifier or credential is recorded here.
- T30 is **reported complete by the user** for a real Custom Provider. The supplied screen shows Custom Provider selected and saved, an API key registered, and Runtime ready. It also says the connection check has not been executed. That screenshot confirms the saved configuration and key-presence state, but does not show the successful real-provider response; no endpoint or key value is recorded here.
- T31 is **partial**. Actions run [36578182205](https://github.com/hide212131/hane/actions/runs/36578182205) at exact product head `4f3d411` passed macOS and Windows workspace tests and Clippy, including macOS-specific tests. Documentation-only head `a201598` run [36580028392](https://github.com/hide212131/hane/actions/runs/36580028392) also passed macOS and Windows workspace tests and Clippy. In the isolated macOS app, the AI page displayed real OAuth state, seven models, and the saved GPT-6-Luna selection; the user changed the unsaved selection twice and the fixed Probe displayed success with `HANE_AI_OK`. Windows GUI, Japanese IME, and full navigation/focus scenarios remain unverified. PR #397 remains Draft; no CodeRabbit review was triggered.
- Local verification at this source state: `cargo build -p hane` **pass** (`/tmp/hane379-p5-20260929/cargo-build-hane-final-precommit.log`); workspace Clippy **pass** (`/tmp/hane379-p5-20260929/cargo-clippy-final-precommit.log`); focused rustfmt for `crates/ui/src/view/ai_settings.rs` **pass** (`/tmp/hane379-p5-20260929/rustfmt-ai-settings-final-precommit.log`); `git diff --check` **pass**. After the manual Probe result, `cargo test -p hane-ai` was rerun: **181 passed**, log `/tmp/hane379-p5-20260929/cargo-test-hane-ai-after-probe.log`. `cargo fmt --all -- --check` remains **fail** on repository-wide existing formatting differences; the changed AI settings file does not appear in `/tmp/hane379-p5-20260929/cargo-fmt-all-after-probe.log`.

### AI settings clarity follow-up (working changes after `af6a32e`)

- The supplied screens show the saved connection (`Custom Provider`), ChatGPT account state (`未確認`), and API key status (`登録済み`) separately. They also show the fixed connection check as not run at capture time.
- `crates/ui/src/view/ai_settings.rs` now names the saved and currently selected connections separately; explains that choosing a provider only changes the draft; says saving applies the selected connection; and distinguishes provider selection from login and connection testing.
- The Custom Provider section now labels the no-key action `API keyを登録` and the existing-key action `API keyを置き換える`. On selection, the page explains that the key field appears, the user enters the key, then presses the bottom `保存` button. It says the value stays hidden and connection testing is separate. The API-key button/input wrapper is exposed to the UI test harness.
- `view::ai_settings::tests::connection_and_api_key_choices_explain_when_they_take_effect` and `view::ai_settings::tests::register_api_key_button_reveals_the_masked_input` pass as part of `cargo test -p hane-ui` (**220 passed**); log `/tmp/hane379-p5-20260929/cargo-test-hane-ui-ux-final.log`. `cargo test -p hane-ai`: **181 passed**; log `/tmp/hane379-p5-20260929/cargo-test-hane-ai-ux-final.log`.
- `cargo build -p hane`, workspace Clippy, focused rustfmt for `crates/ui/src/view/ai_settings.rs`, and `git diff --check`: **pass**. `cargo fmt --all -- --check`: **fail** on existing formatting differences in other files; `ai_settings.rs` is absent from `/tmp/hane379-p5-20260929/cargo-fmt-all-ux-final.log`.

### Issue #410 UI implementation follow-up (2026-09-30; product commit `d64f9f259b723c02cbc1a9d402ba87663e3a443c`)

At start of this follow-up, Issue #379 was Open; PR #397 was Draft/Open at `217139dc0b049d69db801649cb536c1e37d83821`; the remote implementation branch matched that head. Design PR #392 remained Open at `025c53d0d03a9ecc2a221d80257d88e59b6c5dfd`. UI specification PR #410 remained Open/non-Draft at `26178e13df11d572a814a9b7af30f7c6756bfb54`. PR #397 had no submitted review and its CodeRabbit check said review skipped for a Draft PR. Current main was `5be310c6136e662119e2d208322f832b317e14c4`; PR #397 still targets its earlier recorded base. No rebase or change to the design-only PR was made.

This product-only change preserves the Issue #379 service/runtime/auth contracts and applies the visible structure from #410:

- The AI page is centered and scrolls above a fixed save bar. It presents `現在有効な設定`, draft status, two radio-style connection cards, only the selected connection form, `応答を確認`, then collapsed diagnostics.
- The Custom Provider form presents AI service and model together, an API-key status row with `登録`/`変更`/`削除` and `元に戻す`, a masked key editor with `やめる`, and collapsible `接続先の詳細`. It has no “keep key” action. The key value is never read back.
- ChatGPT model selection now uses a dropdown with a separate manual model-ID field. The user can select multiple models in the same unsaved edit. Account status and login actions are grouped, with the required saved-active-ChatGPT prerequisite explained.
- Probe target, fixed-message/privacy/cost information, status, and a current-only result are grouped together. Stale probe text is not presented as the current connection result. Runtime diagnostics are collapsed by default. Save bar messages distinguish dirty, saving, incomplete, unapplied, and applied settings without claiming that an unconfirmed runtime is active.
- T29 and T30 remain **reported complete by the user** as recorded in the earlier section. This follow-up did not redo real OAuth or a real Custom Provider connection. A temporary macOS app was opened for the user to inspect the updated screen; its state root and Markdown workspace are isolated under `/tmp/hane379-ui-410-20260930-v2`. The user has not yet reported a manual result for this latest preview. Windows GUI and native Japanese IME/Tab checks remain outstanding.

#### Acceptance evidence at product commit `d64f9f2`

- **T24 — partial:** UI structure and actions above are in place; `view::ai_settings::tests::ai_settings_show_only_the_selected_connection_and_keep_savebar_visible`, `view::ai_settings::tests::register_api_key_button_reveals_the_masked_input`, `view::ai_settings::tests::chatgpt_model_selection_can_be_changed_multiple_times_before_saving`, and `view::ai_settings::tests::fixed_probe_button_submits_a_probe_command` pass. Real account/provider success is not inferred from these UI tests. Evidence: `/tmp/hane379-ui410-20260930/workspace-test.log`.
- **T25 — partial:** the masked key editor can be canceled without saving; secret masking/editor shortcut regression test remains passing. Native Japanese IME and Tab/focus behavior are unverified. Evidence: `view::ai_settings::tests::register_api_key_button_reveals_the_masked_input`, `view::ai_settings::tests::ai_secret_input_is_masked_and_editor_shortcuts_do_not_reach_document`; same workspace log.
- **T26/T27/T28 — unchanged prior evidence:** their existing asynchronous external-open, document isolation, and recovery UI results remain as recorded above. The product change did not expand those claims.
- **T29/T30 — user-reported complete; not rerun here:** see the supplied-screen qualification in the prior P5 GUI follow-up. The current isolated preview does not perform OAuth or contact a provider.
- **T31 — partial:** local workspace/macOS CI-equivalent checks pass below; user inspection of the latest preview is pending and Windows GUI remains unverified.

#### Local verification at this source state

- `cargo test --workspace --all-features --locked`: **pass**, including 221 `hane-ui` tests; log `/tmp/hane379-ui410-20260930/workspace-test.log`.
- `cargo test -p hane-ai`: **pass**, 134 unit + 11 account-service + 16 Custom Provider runtime + 20 lifecycle; log `/tmp/hane379-ui410-20260930/hane-ai-test.log`.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: **pass**; log `/tmp/hane379-ui410-20260930/clippy.log`.
- `cargo build -p hane`: **pass**; log `/tmp/hane379-ui410-20260930/build-hane.log`.
- macOS-specific CI tests: **pass** — `hane_oblique` (2 tests) and `hane_input_source` (1 test); logs `/tmp/hane379-ui410-20260930/macos-hane-oblique.log` and `/tmp/hane379-ui410-20260930/macos-input-source.log`.
- `rustfmt --edition 2024 --check crates/ui/src/view/ai_settings.rs` and `git diff --check`: **pass**.
- Required `cargo fmt --all -- --check`: **fail** on repository-wide existing differences in 14 files, including unrelated sections of `crates/ui/src/view.rs` at lines 1074 and 10678+; the changed AI settings file is not in the report, and the edited settings-layout hunk in `view.rs` is not in the report. Full output: `/tmp/hane379-ui410-20260930/cargo-fmt-check.log`. Unrelated files were not reformatted.

The UI source does not alter the standalone Codex safety profile; the scoped T00 result and limits remain unchanged above. PR #397 remains the implementation PR, Draft/Open; no CodeRabbit trigger, Draft removal, merge, or Issue close was performed.

### GitHub Actions result for Issue #410 UI follow-up

- Actions run [36603564620](https://github.com/hide212131/hane/actions/runs/36603564620), exact submitted head `df432dab4ed18a1b40f3de310a7eaa0e30f1f88a`: **pass** for the macOS and Windows `cargo test / clippy` jobs. This run includes product commit `d64f9f2` and the UI evidence record. It is CI evidence, not native GUI evidence.
- CodeRabbit's status context reported `Review skipped: draft pull request`; this was not a review. No manual review trigger or Draft-state change was made.

## 2026-09-30 UI layout follow-up after user review (product commit `481c4141887b96b5e47f68f8a024c4be7a1c4520`)

The user reported that the native Hane page still differed substantially from the HTML in PR #410. This follow-up keeps the existing implementation PR #397 and narrows the visible product UI toward the HTML's hierarchy. It does not change the Hane App Server contract or the #392 design-only PR.

- At start, Issue #379 was Open; PR #397 was Draft/Open at `a94609be0d1a2a5b55bda050f1485e1aef5b4fd5`; PR #392 was Open at `025c53d0d03a9ecc2a221d80257d88e59b6c5dfd`; PR #410 was Open/non-Draft at `26178e13df11d572a814a9b7af30f7c6756bfb54`. The implementation branch matched its remote at `a94609b`; no uncommitted user changes were present. Current main was last observed at `5be310c6136e662119e2d208322f832b317e14c4`, and PR #397 still targets base `864802015ba943f4c68108a6c2cb88d557195a07`.
- `現在有効な設定` is now a compact card with method, destination and model values, a status badge, and one short current-state message. Runtime details and generation values stay in the collapsed diagnostics section. Account/login state remains in the ChatGPT form, and API-key state remains in the API-key form.
- The page title includes the HTML's small accent icon. Connection choices share a two-column row when the window has room. The selected ChatGPT/API-key form has a separate panel background. The response heading is outside its bordered test panel; target, privacy/cost note, action, and non-default result state stay together. `応答確認: まだ実行していません` is no longer redundantly rendered below the button before a test is run.
- The existing Hane settings navigation remains the product chrome. The HTML's preview controls, demo sidebar, and sample-only controls were not copied. The current `ProbeResult` contract has no timestamp, so this UI does not invent a confirmation time.
- T00 remains **pass within its documented standalone 0.157.1 scope**; this is a UI-only change and does not broaden or alter that evidence.

### Acceptance evidence for product commit `481c414`

- **T24 — partial:** `cargo test -p hane-ui --locked` passed all **221 tests**; the existing `view::ai_settings::tests` for selected forms, savebar, key registration/cancel, probe submission, and repeated model selection remain in the suite. This is automated GPUI evidence, not manual native-GUI review. Log: `/tmp/hane379-ui-410-20260930-v3/logs/cargo-test-hane-ui.log`.
- **T25–T28 — unchanged prior evidence:** UI code in this follow-up does not change editor-input, external-open, document-session, or recovery contracts. Their existing partial/pass scope remains as in the sections above; native IME/Tab/focus scenarios remain unverified.
- **T29/T30 — user-reported complete; not rerun:** no OAuth or real provider operation was performed in this UI-only follow-up. The preview has no API key and uses isolated state.
- **T31 — partial:** the new app was built and launched for the user to inspect at `/tmp/hane379-ui-410-20260930-v3/Hane-UI-Review.app`, with state `/tmp/hane379-ui-410-20260930-v3/state` and a disposable Markdown workspace `/tmp/hane379-ui-410-20260930-v3/workspace`. The built executable SHA-256 is `346722eb0fbfa8a5200d6a4f58efc1ee7b62a90e2529ddadd94c1e8f781aa2df`. The screen-capture tool reported macOS locked, so no native visual inspection by the agent is claimed; the user's own inspection of this latest preview is pending. No Windows GUI or Japanese IME/Tab scenario was run.
- `cargo test -p hane-ai --locked`: **pass**, 181 tests (134 unit, 11 account-service, 16 Custom Provider runtime, 20 runtime lifecycle). Log: `/tmp/hane379-ui-410-20260930-v3/logs/cargo-test-hane-ai.log`.
- `/Users/hide/.cargo/bin/cargo build -p hane --locked` with the repository-pinned Rust 1.98.1: **pass**. Log: `/tmp/hane379-ui-410-20260930-v3/logs/cargo-build-hane.log`.
- `/Users/hide/.cargo/bin/rustfmt --edition 2024 --check crates/ui/src/view/ai_settings.rs`: **pass**; `/Users/hide/.cargo/bin/cargo fmt --all -- --check`: **fail** on existing repository-wide formatting differences, with no `ai_settings.rs` entry. Logs: `/tmp/hane379-ui-410-20260930-v3/logs/rustfmt-ai-settings.log` and `/tmp/hane379-ui-410-20260930-v3/logs/cargo-fmt-check.log`.
- `git diff --check`: **pass**. Log: `/tmp/hane379-ui-410-20260930-v3/logs/git-diff-check.log`.

GitHub Actions and PR body updates for the pushed current head will be recorded in the following section. PR #397 remains Draft/Open; no CodeRabbit trigger, Draft removal, merge, or Issue close was performed.
