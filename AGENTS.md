# エージェント共通の指示

## GitHub 上の文章

- 作成・更新する Issue / Pull Request のタイトル・本文は日本語で書く。
- コメント、レビュー本文、インラインの指摘、返信、進捗報告、判断理由、引き継ぎも日本語で書く。入力が英語でも説明・要約は日本語にする。
- コード、コマンド、パス、URL、製品名、ログの引用は必要に応じて原文を保持する。
- 機械処理用のコマンド、ラベル、JSON キー・列挙値、相関マーカーは翻訳しない。JSON の人向けの説明・判断理由は日本語にする。
- 投稿前に日本語の説明と機械処理用の識別子が両立していることを確認する。

詳細は [開発ワークフローの言語方針](docs/agentic-development-workflow.md#issuepull-requestコメントの言語) を参照する。

## Issue の目的と完了条件

Issue に明示された目的・受入条件を、レビュー指摘の全件解消より優先する。`review findings = 0` を Pull Request の完了条件にはしない。merge blocker とするのは、P0 / P1、セキュリティ・データ破壊・権限逸脱、通常経路で再現する明確な不具合、CI failure、または Issue の目的・受入条件を直接満たせなくする P2 とする。rare race、複合障害、極端な rerun / recovery など、元 Issue の成立を直接妨げない指摘は原則として follow-up Issue に分離し、現在の Pull Request の scope を無期限に拡大しない。追加修正に入る前に「元 Issue を完了するために必要か」を確認し、不要なら current PR には含めない。

## 設計と実装の役割分担

Hane の agentic development workflow は、要求整理・詳細設計を担当する Work（ChatGPT）、実装を担当する Claude Code、レビューを担当する Codex、進行判断を担当する GitHub Copilot の四者に役割を分ける。Work は製品実装（ソース・テスト・ビルド設定の変更、branch 作成、commit / push、Pull Request 作成）には進まず、Issue が実装可能な状態になった時点で設計作業を終了する。初回実装は、権限を持つ利用者による本文完全一致の `/implement` コメントで開始する。実装まで依頼済みなら Work が利用者の権限で重複確認後に投稿してよく、設計のみの依頼では投稿しない。PR 作成後の修正は既存の Copilot 判定と Claude fix の経路に従う。

役割分担の正本は [開発ワークフロー](docs/agentic-development-workflow.md) とし、採用理由は [ADR-0023](docs/adr/0023-ai-agent-development-workflow.md)・[ADR-0026](docs/adr/0026-work-design-handoff.md) に残す。設計 Issue を作成するときは [Issue テンプレート](.github/ISSUE_TEMPLATE) を使う。
