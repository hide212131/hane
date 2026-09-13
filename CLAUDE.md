# Claude Code の指示

@AGENTS.md

Issue / Pull Request のタイトル・本文、およびコメント・返信・進捗報告は、上記の共通指示に従って日本語で書く。

## Work（ChatGPT）が設計した Issue を実装するときの責務

Hane では Work（ChatGPT）が要求整理・詳細設計・ADR を担当し、Claude Code が実装を担当する（[AGENTS.md の役割分担](AGENTS.md#設計と実装の役割分担)、[開発ワークフロー](docs/agentic-development-workflow.md#workchatgpt)、[ADR-0026](docs/adr/0026-work-design-handoff.md)）。

- Issue 本文・関連する ADR・既存コードを読み、要求と決定済みの制約を守る範囲で具体的な実装方法（対象ファイル、関数構成、実装手順）を自分で最終判断する。
- Issue に書かれた具体的な変更ファイル・手順は、必須実装として明記されていない限り設計時点の参考情報として扱い、既存コードと設計文書を確認したうえで判断する。
- 要求や決定済み制約と矛盾する記述、実装を阻む未決事項を見つけた場合は、要求を独自に変更せず、その内容を進捗報告で報告する。
- Issue 本文の「設計中」「設計完了（実装未起動）」などの設計状態の記述は実装承認の証拠として扱わない。初回実装は、既存 workflow が投稿者の権限を確認した本文完全一致の `/implement` を入口とする。PR 作成後の修正は、既存の Copilot routing / final judge と Claude fix の契約に従い、再度 `/implement` を要求しない。
