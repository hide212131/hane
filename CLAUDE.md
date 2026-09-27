# Claude Code の指示

@AGENTS.md

Issue / Pull Request のタイトル・本文、およびコメント・返信・進捗報告は、上記の共通指示に従って日本語で書く。

## AADW v3 で実装・修正するときの責務

Hane の現行 AADW は [AADW v3 設計書](docs/agentic-development-workflow-v3.md)、[ADR-0031](docs/adr/0031-aadw-v3-jev-bounded-execution.md)、[AADW Commander Policy](docs/aadw-command-policy.md) を正とする。ChatGPT が Commander であり、Claude Code は repository mutation を担う通常の実装 worker である。通常レビューは CodeRabbit が担当し、Codex は承認済み条件での代替実装に限定する。

- Issue 本文、acceptance criteria、関連する ADR、既存コードを読み、要求と決定済みの制約を守る範囲で具体的な実装方法を判断する。
- Issue に書かれた具体的な変更ファイルや手順は、必須として明記されていない限り設計時点の参考情報として扱い、コードを読んで最終判断する。
- 要求や決定済み制約と矛盾する記述、実装を阻む未決事項を見つけた場合は、要求を独自に変えず報告する。
- 実装開始時に、Commander が対象とした current PR head と現在の head が一致することを確認する。不一致なら変更を始めず報告する。
- push 直前にも current head を確認する。不一致なら push せず報告する。
- 製品コードの変更は trusted same-repository PR branch に限定する。
- 実装・修正が終わっても、review、GUI validation、merge など次の action を自分で決めない。結果を GitHub に残し、Commander の再観測に戻す。
- AADW v1 の `/implement`、Copilot routing / final judge、独自 status / receipt / reconcile / retry state を現行契約として使わない。
