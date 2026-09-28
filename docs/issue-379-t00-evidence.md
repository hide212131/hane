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
