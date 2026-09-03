# AI Agent Development Workflow

## 目的

Hane の Issue から実装、レビュー、修正、マージ判断までを、複数の coding agent に役割を分けて自動化する。

この文書を運用設計の正本とする。役割分担そのものを採用する理由は
[ADR-0023](adr/0023-ai-agent-development-workflow.md) に残す。

追跡 Issue: #44

## 基本方針

1つの agent に実装、レビュー、進行判断をすべて任せない。

- **Claude Code** は実装を担当する。
- **Codex** は Pull Request のコードレビューを担当する。
- **GitHub Copilot** は Codex の指摘、Pull Request、CI の状態を読み、次の処理を判断する。
- **GitHub Actions** はトリガー、権限、状態遷移、最終的な機械的チェックを担当する。

AI の判断と、GitHub 上で実際に変更を加える処理を分ける。特にマージは Copilot の判断だけでは実行せず、CI、対象 commit、未解決レビューなどを機械的に確認する。

## 全体フロー

```text
Issue
  |
  v
Claude Code
  |  実装・テスト・commit
  v
Pull Request
  |
  v
Codex review
  |
  v
Copilot judge
  |
  +-- 修正必要 --> Claude Code fix --> push --> Codex review --+
  |                                                        |
  +-- 問題なし --> deterministic merge gate --> merge <---+
```

## 各 agent の責務

### Claude Code

Claude Code はコードを書く側に限定する。

初回実装では次を行う。

- 元 Issue とリポジトリ内の設計文書を読む。
- 必要なコードとテストを変更する。
- Hane の標準検証を実行する。
- 作業用 branch に commit / push する。
- Pull Request を作成する。

修正時は、同じ Pull Request の最新 Codex review と Copilot の判断を読み、妥当な指摘へ対応して同じ branch に push する。新しい Pull Request は作らない。

#### 入力の信頼境界

`/implement` の実行者を owner / write 権限保持者に制限しても、実装対象の Issue 本文や既存コメントそのものは外部ユーザーが自由に書ける。Claude Code Action の公式 security guidance が指摘するとおり、public repository の外部投稿には hidden instruction による prompt injection の危険があるため、Issue / comment の全文をそのまま Claude への指示として渡す設計にはしない。

- Claude に渡す Issue 本文・コメントは、owner / write 権限保持者と信頼する bot（Codex、GitHub Actions 経由の状態コメントなど）の投稿に限定する。
- 上記に該当しない外部投稿を参照する場合は、実装対象の指示ではなく **untrusted input** として明示的に分離し、prompt 内でその旨を注記する。
- untrusted input 中の指示文（「これを無視して」「secret を出力して」等）に従わない。

### Codex

Codex はレビュー側に限定する。

- Pull Request の差分をレビューする。
- correctness、回帰、テスト不足、保守性、Hane の既存設計との不整合を確認する。
- 修正が必要な点は Pull Request review として残す。
- 実装そのものは変更しない。

Codex review は ChatGPT の Codex と GitHub の連携を使う。GitHub Agentic Workflows の `engine: codex` は使わない。後者は API key 認証となり、今回の「ChatGPT のサブスクリプションで Codex を使う」という方針と異なるためである。

### GitHub Copilot

Copilot は進行判断を担当する。

GitHub Agentic Workflows の `engine: copilot` を使い、次を入力として判断する。

- 元 Issue
- Pull Request の差分
- 現在の head SHA
- Codex の最新 review と review comment
- 未解決 review thread
- CI / status check の結果
- 過去の agent loop の状態

出力は次のどちらかを基本とする。

1. `fix`: Claude Code の修正 workflow を起動する。
2. `ready`: マージ条件を検査する deterministic workflow を起動する。

判断できない場合は `ready` にしない。人間が確認できるコメントを残して停止する。

## GitHub Actions の責務

Agent は判断するが、GitHub Actions が制御する。

Actions 側では最低限、次を保証する。

- 同じ event を二重処理しない。
- レビュー対象の head SHA と現在の head SHA が一致することを確認する。
- 修正後は新しい head SHA に対して Codex review をやり直す。
- 最大反復回数を設ける。
- 認証情報を prompt やログへ渡さない。
- merge 前に Hane の必須チェックを再確認する。

## 認証

### Claude Code

Claude Code Action を直接使う。Claude Pro / Max のサブスクリプションを使う場合は、ローカルで
`claude setup-token` を実行して OAuth token を生成し、GitHub Actions Secret
`CLAUDE_CODE_OAUTH_TOKEN` に保存する。

workflow からは `anthropics/claude-code-action` の `claude_code_oauth_token` に Secret を渡す。

GitHub Agentic Workflows の Claude engine は、この OAuth token を使う方式とは別物であり、今回の Claude 実装経路には使わない。

### Codex

ChatGPT アカウントで Codex にサインインし、Codex Web / GitHub 側で Hane リポジトリを接続する。
Pull Request review はこの接続を使う。

今回の Codex review では `OPENAI_API_KEY` を GitHub Actions に置かない。

### GitHub Copilot

Hane は個人所有リポジトリなので、GitHub Agentic Workflows の Copilot inference には
`COPILOT_GITHUB_TOKEN` を使う。これは Copilot Requests 権限を持つ fine-grained Personal Access Token とし、GitHub Actions Secret に保存する。

`GITHUB_TOKEN` に `copilot-requests: write` を与えて組織課金する方式は、組織所有リポジトリ向けなので今回の前提にはしない。

## 状態管理

無限ループと古いレビューの再処理を避けるため、状態は Pull Request ごとに管理する。

最低限、次を保持する。

- 対象 Pull Request 番号
- 現在の head SHA
- Codex がレビューした head SHA
- Copilot が判定した head SHA
- 現在の状態
- 修正反復回数

状態候補は次のとおり。

```text
implementing
waiting-codex
waiting-judge
fix-requested
ready-to-merge
blocked
merged
```

実装時には、機械可読な Pull Request comment または GitHub の別の永続領域に保存する。ラベルは人間向けの表示に使ってよいが、ラベルだけを正本にはしない。

## トリガー

初期案では次のイベントを使う。

### 実装開始

Issue 上の明示的なコマンドで始める。

```text
/implement
```

Issue 作成だけでは自動実装を始めない。誤作動と意図しないコスト消費を防ぐためである。

Hane は公開リポジトリであり、`/implement` コメントの文字列だけを起動条件にすると、任意の第三者が Claude の実行枠と書き込み権限を起動できてしまう。dispatch 前に、コメント投稿者の実効権限を検証し、owner / write 権限保持者以外からの `/implement` は無視する。これを不変条件とする。

`author_association` の `MEMBER` は組織所属を示すだけで、そのリポジトリへの write 権限を保証しない。collaborator に read／triage のみを与えることもできるため、`author_association` を write 権限の代用にはしない。代わりに `GET /repos/{owner}/{repo}/collaborators/{username}/permission` などで現在の実効権限を取得し、`write` / `maintain` / `admin` の場合のみ許可する。Hane は個人所有リポジトリのため、`author_association` が `OWNER` の場合を明示的に許可する最適化は行ってよい。

### Codex review

Claude Code が Pull Request を作成したとき、または修正 commit を push したときに、その最新 head SHA を対象としてレビューする。

Codex の GitHub integration による自動 review は新規 Pull Request 作成時のみ保証される公式仕様であり、修正 push 後の再レビューは保証されない。また指摘がない場合は review 本文なしの 👍 reaction のみになることがある。そのため、修正 push 後の再レビューは Actions から `@codex review` コメントを明示的に投稿する方式で行い、次の契約を満たす。

- Actions が `@codex review` を投稿する際、投稿コメント（またはそれに紐づく Pull Request comment）に対象 head SHA を機械可読な形で記録する。
- Codex の review／reaction を、その記録した head SHA に対応付けて状態管理に保存する。
- Codex は手動レビューの実行中にリクエストコメントへ 👀 reaction を付け、その後にレビューを投稿する。この 👀 は実行中（running）を示すだけであり、完了とはみなさない。
- Copilot judge が読む「Codex review 完了」は、記録した head SHA を対象とする submitted review、または指摘なしを示す終端 reaction（👍）のいずれかが観測できた場合のみ true とする。👀 のみの状態では起動しない。
- 一定時間内に上記の完了イベントを観測できない、または head SHA の対応付けが判定できない場合は「未完了」として扱い、fail closed で `ready` に進めない。

### Copilot judge

Codex review が完了したことを検出して起動する。

GitHub Agentic Workflows から許可する書き込みは必要最小限にする。Claude 修正 workflow を起動する場合は allowlist した `dispatch-workflow` を使う。

## 修正ループ

Copilot が `fix` と判断した場合は、Claude Code に次の情報を渡す。

- Pull Request 番号
- 修正対象の head SHA
- Codex review / comment
- Copilot が修正必要と判断した理由

Claude は修正、検証、push まで行う。push 後は以前の Codex review を再利用せず、新しい head SHA に対してレビューをやり直す。

最大反復回数の初期値は **3回** とする。3回で収束しない場合は `blocked` とし、人間へ引き継ぐ。

## マージ条件

Copilot の `ready` は「マージしてよい」という最終権限ではなく、機械的なマージ判定へ進める合図とする。

少なくとも次をすべて満たした場合だけマージする。

- Copilot が最新 head SHA に対して `ready` と判断している。
- Codex review の対象 SHA が最新 head SHA と一致する。
- 必須 CI が成功している。
- Pull Request が draft ではない。
- conflict がない。
- 未解決の blocking review thread がない。
- 判定後に head SHA が変わっていない。

Hane の標準検証は README に従い、少なくとも次を必須とする。

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

現在のリポジトリには `pull_request` トリガーで動く CI、branch protection、ruleset がなく、必須チェックは0件である。この状態で「必須 CI が成功している」を実装すると、必須チェックの空集合を成功扱いして未検証の変更を自動マージしてしまう。そのため導入手順で次を満たす。

- `pull_request` トリガーで上記の `cargo test` / `cargo clippy` を実行する CI workflow を追加する。
- merge gate が要求する check name を明示的なリストとして定義する（空リストを許可しない）。
- 要求した check が0件、`missing`、`skipped`、または結果が取得できない場合は成功とみなさず、`blocked` として拒否する。branch protection / ruleset による required status checks の設定もあわせて行う。

GitHub Agentic Workflows の `merge-pull-request` safe output は現時点で experimental であり、デフォルト branch を対象とする merge に制約があるため、初期実装では直接採用しない。Copilot は `ready` 判定までを行い、通常の GitHub API を使う別の deterministic merge gate に渡す。

## セキュリティ

- OAuth token、Personal Access Token、API key はすべて GitHub Actions Secrets に置く。
- Secret を Issue、Pull Request、comment、agent prompt に展開しない。
- Agentic Workflow の agent job は read-only を基本とする。
- 書き込みは safe outputs と明示した worker workflow に限定する。
- fork 由来の Pull Request では Secret を使う workflow を自動実行しない。
- workflow file 自体の変更を含む Pull Request は自動マージ対象外とする。

## 導入順序

### Phase 1: 設計と認証

- この文書と ADR を追加する。
- Claude Code / Codex / Copilot の認証を1回ずつ設定する。
- Agentic Workflows 用 CLI (`gh aw`) を導入する。
- `pull_request` トリガーで `cargo test` / `cargo clippy` を実行する必須 CI workflow を追加し、branch protection / ruleset で required status checks として設定する。

### Phase 2: Claude 実装

- `/implement` コメント投稿者の実効 repository permission（collaborator permission API、または `OWNER`）を検証し、write 権限保持者以外は無視する。
- `/implement` から Claude Code を起動する。
- Issue を実装して Pull Request を作れるところまで通す。
- Claude に渡す Issue / comment を、信頼できる投稿者のものと untrusted input に分離する。

### Phase 3: Codex review

- Pull Request の最新 head SHA に Codex review を実行する。
- review 完了を次の処理へ渡せるようにする。

### Phase 4: Copilot judge

- Agentic Workflow で Codex review、CI、Pull Request を読む。
- `fix` / `ready` / `blocked` を判断する。
- `fix` なら Claude 修正 workflow を dispatch する。

### Phase 5: merge gate

- SHA、CI、review thread、mergeability を機械的に確認する。
- 条件を満たした Pull Request だけを squash merge する。
- 失敗時は自動マージせず `blocked` にする。

## 参考

- GitHub Agentic Workflows: https://docs.github.com/en/copilot/concepts/agents/about-github-agentic-workflows
- Agentic Workflows authentication: https://github.github.com/gh-aw/reference/auth/
- Agentic Workflows safe outputs: https://github.github.com/gh-aw/reference/safe-outputs/
- Claude Code Action setup: https://github.com/anthropics/claude-code-action/blob/main/docs/setup.md
- Codex with a ChatGPT plan: https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan
