# ADR-0026: Work を上流の planner/designer とし、Claude Code への明示的な handoff を定義する

## ステータス

採用

## 日付

2026-09-11

## 関連

- Issue #124
- ADR-0023: AI agent 開発を実装・レビュー・進行判断に分離する（このADRが対象とする三者分離を置き換えない）

## 背景

ChatGPT / Work で要求整理から詳細設計まで進めると、そのまま実装まで進んでしまい、Claude Code を実装担当とする Hane の運用（[ADR-0023](0023-ai-agent-development-workflow.md)）との境界が曖昧になっていた。設計工程の責務、終了条件、実装へ渡す成果物、明示的な起動手順が共通指示に定義されておらず、設計案がそのまま実装手順を強制するのか、決定済みの制約に留まるのかも区別できていなかった。

## 決定

Hane の agentic development workflow に、ADR-0023 が定義する実装・レビュー・進行判断の三者に加えて、上流の設計工程を担う四人目の役割を明文化する。

- **Work（ChatGPT）**: 要求整理・調査・詳細設計・ADR・受け入れ条件の整備までを担当する。実装対象のソースコード・テストコード・ビルド設定を変更せず、製品実装のための branch 作成・commit / push・Pull Request 作成には進まない。Issue が実装可能になった時点で設計作業を終了する。調査のためのコード閲覧・検証、依頼された設計文書・ADR の整備は許容する。
- **Claude Code**: 引き続き実装を担当する（ADR-0023 のとおり）。Issue・ADR・既存コードを読み、要求と決定済み制約を守る範囲で具体的な実装方法を最終判断する。矛盾や実装を阻む未決事項があれば報告し、要求を勝手に変えない。
- **Codex** / **GitHub Copilot** / **GitHub Actions**: ADR-0023 のとおり、レビュー・進行判断・状態遷移と権限制御を担当する。役割・契約は変更しない。

具体的な責務範囲、終了条件、禁止範囲、再利用可能なタスク指示、design handoff の書式は [`docs/agentic-development-workflow.md`](../agentic-development-workflow.md) を正とする。ADR-0023 が定義する **Claude = implementer / Codex = reviewer / Copilot = judge** の判断は置き換えず、その上流に Work の設計工程と Claude Code への明示的な handoff を補足する。

### 設計状態はドキュメントの記述で表す

設計状態は Issue 本文の「設計中」「設計完了（実装未起動）」等の人向け記述で表す。新しい ready-for-implementation ラベルや機械可読な状態は追加しない。

- 標準ラベル、`gui-validation-required`、`agentic-auto-merge` などの既存ラベル運用は変更しない。
- PR state machine（`implementing` / `waiting-*` / `fix-requested` / `ready-to-merge` / `blocked` / `merged`、[docs/agentic-development-workflow.md#状態管理](../agentic-development-workflow.md#状態管理)）は Pull Request と head SHA の検証状態を表すものであり、Issue の設計状態とは分離したまま扱う。

### 実装は明示的な `/implement` でのみ開始する

Issue 作成・設計完了の記述・ラベル操作だけでは実装を開始しない。この不変条件は ADR-0023 の運用を継続するものであり、変更しない。

- 実装を依頼された場合は、既存コメント・Pull Request・進行中の Actions run を確認し、重複起動を避ける。
- 権限を持つ利用者の文脈から、本文完全一致の `/implement` を単独コメントとして投稿する。
- handoff の説明（設計状態、成果物、制約、未決事項の有無など）は Issue 本文または別コメントに置き、`/implement` コメント本文には含めない。
- 設計のみを依頼された場合は起動しない。既に実装まで依頼されていれば、設計完了の記述だけを理由に再承認を求める必要はない。
- 既存 Issue はテンプレートや新しい状態表記を持たなくても、既存の `/implement` 手順でそのまま利用できる。

### ガードは共通指示に置き、workflow は変更しない

このガードは agent の行動指示であり、OS 権限などによる強制的な隔離ではない。Work のローカルな操作を GitHub Actions から技術的に防止することはできない。一方で、既存の Actions は許可された `/implement` の明示性（本文完全一致・投稿者の実効権限検証・Issue 単位の concurrency・既存 PR 確認）をすでに強制しているため、この決定の実施にあたって `.github/workflows` の YAML・script・状態遷移の変更は行わない。

## 結果

- 設計工程と実装工程の境界が、共通指示（AGENTS.md）・Claude Code への指示（CLAUDE.md）・Issue テンプレートという、実際に読み込まれる文書に明示される。
- ADR-0023 の三者分離と `/implement` の起動契約（本文完全一致、権限検証、重複起動防止）を変更せずに、上流の設計工程を追加できる。
- 新しいラベル・機械状態・workflow 変更を追加しないため、既存の PR 状態管理、review fallback、GUI 判定、merge gate、権限制御との非互換を生まない。
- 設計完了の記述は実装承認や merge-ready の証拠として扱われない。実装開始の唯一の起動条件は、権限を持つ利用者による本文完全一致の `/implement` コメントのままである。

## 棄却した案

### 新しい ready-for-implementation ラベルまたは機械可読な設計状態を追加する

Issue の設計状態を GitHub のラベルや別の機械可読な状態として持たせる案は採用しない。Hane の状態管理は PR の head SHA を中心にした検証済み状態（CI・review・GUI validation・judge の終端結果）を正本とする設計であり、Issue 側に新しい機械状態を追加すると、実装開始の起動条件（`/implement` の明示投稿）が別の状態からも起動しうるように誤解されやすくなる。人向けの文章表記で足りるため、この複雑さは避ける。

### `.github/workflows` を変更して Work の関与を強制的に制限する

Work（ChatGPT）はリポジトリの GitHub Actions の外側で動作するため、workflow 側の変更では Work のローカルな設計作業や誤操作を防げない。実装開始の唯一のトリガーである `/implement` の権限検証・重複起動防止・Issue 単位の concurrency は既存の `implement.yml` がすでに強制しており、この決定はその契約を利用するだけで足りる。ADR-0023 の三者分離自体も変更しないため、workflow の変更は不要と判断する。
