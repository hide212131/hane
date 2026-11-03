# Issue #591 画像貼り付けの実装準備

- Issue: https://github.com/hide212131/hane/issues/591
- 基準main: `1e0d345e46d8c1805659968f8fad1148b0ae37a8`
- ブランチ: `codex/issue-591-image-paste`
- 詳細設計: [画像貼り付け](../design/issue-591-image-paste.md)
- 関連採用済み設計: [#596 clipboard](../design/issue-596-rich-clipboard.md)

Claude実装待ちの準備PR。文書差分のみで、製品コードとテストは未変更。実装・GUI・CI・reviewは未完。

今回の範囲はmacOS/Windowsの実クリップボード画像1枚を、文書隣接の自動作成imagesへPNG保存し、通常Markdown参照で本文表示すること。未保存draft、衝突回避、保存失敗、1Undo/Redo、非同期鮮度、Save Asの生成画像コピーも受入対象。利用者が文書隣接imagesを選択済み。

PNG/JPEGファイルのドロップは#591全体に残す後続範囲。設定・不要画像削除・加工・遠隔取得・#596 HTML変換を混ぜない。`Refs #591`で追跡し本フェーズだけでIssueをcloseしない。

実装担当はcurrent main、PR exact head、同責務PRを再取得し、設計第5節の共有状態と第6節の受入対応を確認する。通常の意味判断はJev、製品変更はClaude、Commanderは客観guardと一actionの実行を担当。開始時・push直前のexact-head guardを維持する。

本検討チャットは実装依頼を投稿しない。別PR担当チャットがdefault branchのCommander PolicyとClaude実行手順を読み、Conversationへ一度だけ正規依頼を投稿しprepare成功まで受理を確認する。実装以降の運用も同担当へ渡す。Windows GUIは許可済みWindowsチャットを使う。
