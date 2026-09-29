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
- T29 is **partial**. In the isolated macOS app, a real ChatGPT OAuth sign-in was displayed as signed in and the account API returned seven models. The user selected GPT-6-Luna, changed the unsaved selection to GPT-6-Sol and back to GPT-6-Luna, then saved it. The user then ran the fixed Probe; the app displayed “最後の固定入力確認は成功しました。” and `HANE_AI_OK`. Logout followed by re-login and the subsequent model refresh remain outstanding. No account identifier or credential is recorded here. This is direct app evidence, not the dispatch-only automated test.
- T30 remains **not run** against a real Custom Provider; a real Base URL/model and direct API-key entry in Hane are still required. Mock-provider tests are not real-connection evidence.
- T31 is **partial**. Actions run [36578182205](https://github.com/hide212131/hane/actions/runs/36578182205) at exact source head `4f3d411` passed macOS and Windows workspace tests and Clippy, including macOS-specific tests. In the isolated macOS app, the AI page displayed real OAuth state, seven models, and the saved GPT-6-Luna selection; the user changed the unsaved selection twice and the fixed Probe displayed success with `HANE_AI_OK`. Windows GUI, Japanese IME, and full navigation/focus scenarios remain unverified. PR #397 remains Draft; no CodeRabbit review was triggered.
- Local verification at this source state: `cargo build -p hane` **pass** (`/tmp/hane379-p5-20260929/cargo-build-hane-final-precommit.log`); workspace Clippy **pass** (`/tmp/hane379-p5-20260929/cargo-clippy-final-precommit.log`); focused rustfmt for `crates/ui/src/view/ai_settings.rs` **pass** (`/tmp/hane379-p5-20260929/rustfmt-ai-settings-final-precommit.log`); `git diff --check` **pass**. After the manual Probe result, `cargo test -p hane-ai` was rerun: **181 passed**, log `/tmp/hane379-p5-20260929/cargo-test-hane-ai-after-probe.log`. `cargo fmt --all -- --check` remains **fail** on repository-wide existing formatting differences; the changed AI settings file does not appear in `/tmp/hane379-p5-20260929/cargo-fmt-all-after-probe.log`.
