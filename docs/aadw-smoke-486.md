# Issue #489 文書限定smokeの準備

- フェーズ: 文書限定smoke
- 基準main: `28c6213a350953a833e16853ce29612b75180f5d`
- 採用済み手順: `docs/aadw-claude-execution.md`
- 判断正本: `docs/aadw-command-policy.md`
- 親Issue #486: このsmokeだけではcloseしない。

## 今回の範囲

Claudeはこのファイルだけを変更する。下記の状態行を、依頼コメントに記載した固定文へ置き換える。コード、workflow、テスト、他の文書は変更しない。

## 状態

準備中。Claudeによる文書限定変更は未実施。

## 未検証事項

prepare受理、Claude session model、model usage、patch内容、trusted finalizer、更新後head、verify、required CIは未確認。準備commitはこれらの証拠ではない。
