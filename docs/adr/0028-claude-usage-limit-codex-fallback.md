# ADR-0028: Claude の利用上限時だけ Codex CLI へ限定 fallback する

## ステータス

採用

## 日付

2026-09-20

## 関連

- Issue #241
- ADR-0027: AADW v2 は ChatGPT Commander と GitHub facts を核にする

## 背景

Hane の通常の実装担当は Claude Code である。しかし Claude Code が usage limit に達した場合、Claude の失敗だけで実装を止めると、current head に対する途中成果と Issue の作業継続機会を失う。

一方、任意の Claude failure を別 provider に送ると、authentication error、model/provider error、max turns、unknown error を隠してしまう。また public repository で self-hosted runner 上の認証済み Codex を fork や未検証 head に対して実行することは許可できない。

## 決定

Claude failure diagnostic が `usage_or_rate_limit` のときだけ、Claude worker が保存した exact-head checkpoint artifact を入力に `workflow_run` fallback を起動する。

fallback は次の条件をすべて満たす場合だけ self-hosted `macOS` + `hane-codex` runner で `codex exec` を実行する。

- trusted Commander handoff を起点とした current source run である。
- failure category が `usage_or_rate_limit` であり、artifact の source run ID / attempt と一致する。
- handoff actor が repository の `admin`、`maintain`、または `write` 権限を持つ。
- 対象 PR が open、base/head とも同一 repository、current head が checkpoint の target SHA と一致する。
- self-hosted 実行直前にも actor と exact head を再確認する。
- Claude worker が保存した `checkpoint.patch` を exact target SHA の clean checkout に適用してから Codex を起動する。
- Codex 実行時に GitHub token や repository write credential を環境へ渡さない。
- Codex は runner に事前配置した hash 固定の専用 permission profile を使う。profile は workspace と明示した handoff/context directory 以外の読み取りを拒否し、`~/.codex/auth.json` などのローカル認証を worker の shell から読めないようにする。
- fallback の変更は `.github/`、agent instructions、Commander Policy、MCP / hook 設定などの保護パスを拒否する。
- Codex 実行後の変更は staged patch として取り出し、別の clean checkout に適用する。Codex が触れた `.git` metadata を認証済み push に再利用しない。
- commit 後、push 直前に current head をもう一度確認する。

Codex は commit / push / 次工程の判断を行わず、trusted finalizer が変更パスと exact head を検査して push する。`authentication`、`max_turns`、`model_or_provider`、`unknown`、診断不能な failure は fallback 対象外とする。

## 結果

- Claude の通常経路は変わらない。
- 利用上限だけを明示的に検出して、同じ current PR head の実装を継続できる。
- 自動 multi-provider routing、検索用の別 index、永続的な AADW state、汎用 retry state machine は追加しない。
- self-hosted runner の認証境界と exact-head guard を維持する。

### self-hosted runner の事前設定

専用 OS ユーザーの `$CODEX_HOME`（未設定なら `~/.codex`）に、次の内容を `hane-codex-fallback.config.toml` として配置する。workflow は SHA-256 `a89a2e5abacc2e13c653d8174d030d5e77fad2062c2e23ad78106dbfe893796d` と一致しない profile を fail closed で拒否する。

```toml
approval_policy = "never"
default_permissions = "hane-codex-fallback"

[permissions.hane-codex-fallback]
extends = ":workspace"

[permissions.hane-codex-fallback.filesystem]
":root" = "deny"
":minimal" = "read"
":tmpdir" = "deny"
":slash_tmp" = "deny"

[permissions.hane-codex-fallback.network]
enabled = false
```

この profile は Codex 本体がローカルの ChatGPT login を使うことを妨げず、Codex が起動する worker command からは認証ファイルを見えなくする。permission profile と旧 `sandbox_mode` は併用しない。専用 `CODEX_HOME` にはこの profile とローカル login の `auth.json` だけを置き、追加の `config.toml` は置かない。runner は専用 OS ユーザーで運用し、profile と auth の所有者・権限を runner 管理者だけに限定する。

## 正本

全体設計は [`docs/agentic-development-workflow-v2.md`](../agentic-development-workflow-v2.md)、判断ルールは [`docs/aadw-command-policy.md`](../aadw-command-policy.md) を正とする。
