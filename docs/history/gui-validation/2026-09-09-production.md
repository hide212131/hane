# 本番GUI・最終判定・自動マージの実証

Issue #44 / #55。実験workflowの成功と、本番workflowで認証した証拠を区別する。

## GUI不要PRの停止と自動マージ

PR #89、対象 `f4137529722f1a946596c7c6bb67d1d45bfb4d7a`。対応済みだが未解決のreview threadを残した状態で、[final run 34291844354](https://github.com/hide212131/hane/actions/runs/34291844354) がCopilot `blocked`、gate `unresolved review threads` を記録した。CLI呼び出し失敗によるblockedとは別の、本物の判断である。

実差分で対応を確認してthreadをresolveし、`agentic-auto-merge`を付与した。[final run 34292028249](https://github.com/hide212131/hane/actions/runs/34292028249) はCopilot `ready`、gate違反なしを確認し、GitHubのexact-head CAS付きsquash mergeを実行した。merge SHAは `77e9e09d413ab7dfa82479d2303289e746dfda8c`。両runのcontrolは `6aa424faacacb18996375da70535de07a2fe5d63`、judge手順は `copilot-final/2`。人手によるマージではない。

## GUI必須PRの本番passと実キャンセル

PR #90、対象 `ebee7f158b930bdd2b4225d9a29dcd25f8e16872`。GUI手順は `hosted-gui-interaction/4`。

- [GUI run 34289845108](https://github.com/hide212131/hane/actions/runs/34289845108)、control `087b9eb081c6f81bc7b34befd7a6f59025c2a2f7`。request → macOS worker → reportが成功し、本番receiptを認証した。ASCII入力・保存・undo/redo・再オープン、日本語IME、OSホイールのfocused smokeがpass。
- [GUI run 34292242965](https://github.com/hide212131/hane/actions/runs/34292242965)、control `6aa424faacacb18996375da70535de07a2fe5d63`。単独のfresh世代をownerが起動し、worker開始とpending登録後に意図的にcancelした。workerはcancelled、reportはsuccess。未完了のworkerを正式なGUI `blocked`として受領し、無期限pendingに残らなかった。
- [final run 34292821169](https://github.com/hide212131/hane/actions/runs/34292821169)、control `40048d38323112fbbae2309a1eeeeeb1b6ff5637`、judge `copilot-final/3`。実cancelのreceiptを受け、CopilotはGUI結果不足を理由に `blocked`、gateも `GUI is not pass` で停止した。

## 判定入力の誤読と修正

[read-only probe 34291848197](https://github.com/hide212131/hane/actions/runs/34291848197) は実際のGUI passを元に、passと明示したfault-injected fail/blockedの3入力をCopilotへ渡した。controlは `6aa424faacacb18996375da70535de07a2fe5d63`。すべて実応答を得たが、passもblockedになった。Copilotが分類の `failure = GUI required` を実行失敗と誤読し、集約済みCI結果とcheck-runを含まないstatus抜粋の意味を誤認したためである。#92でschemaの意味を明記し、judge手順を3へ更新した。gate条件は緩和していない。

模擬fail/blockedは実アプリの故障や実runner中断の証拠ではない。実中断の証拠は上記の別runである。

## 中断からの回復と修正後の判定

[GUI run 34293007271](https://github.com/hide212131/hane/actions/runs/34293007271) は同じ対象SHAのfresh世代として実操作を再実行し、正式なpassへ回復した。controlは `40048d38323112fbbae2309a1eeeeeb1b6ff5637`、GUI手順は4。receiptを実runと照合し、保存証拠16ファイルのSHA-256も照合した。再オープンとスクロール後の画像を目視確認した。

[read-only probe 34293618770](https://github.com/hide212131/hane/actions/runs/34293618770) は同じcontrol、judge手順3で実行した。実passはCopilot `ready`かつgate違反なし、明示した模擬fail/blockedはいずれもCopilot `blocked`かつgate `GUI is not pass` となった。外部書き込みやマージは行っていない。[本番final run 34293573119](https://github.com/hide212131/hane/actions/runs/34293573119) でも実passに対して `ready` を記録した。

opt-in後の [final run 34293722884](https://github.com/hide212131/hane/actions/runs/34293722884) は、CIの個別根拠が省略されていることと、PR本文の古い「準備中はopt-inを無効にする」という記述を理由にblockedとなった。controlは `40048d38323112fbbae2309a1eeeeeb1b6ff5637`、judge手順3。readyを得た後の停止も成功に書き換えず、別の判定として保持する。#93で選択したCIチェックとworkflow結果を判定入力に追加し、手順4へ更新した（control `671ec799db72fb9c29215ef321b8de2da310de92`）。PR本文も、検証済みの証拠と現在のopt-in状態を示す内容へ更新した。gate条件は維持した。

## GUI必須PRの実際の自動マージ

[final run 34295616870](https://github.com/hide212131/hane/actions/runs/34295616870) が、更新後の本文と回復世代 `34293007271-1` の正式passを確認し、Copilot `ready`、gate違反なしで #90 を自動squash mergeした。merge SHAは `fbfe6425453e2bbea87138d79b71a59a355552bf`。人手によるマージではない。

この実行は#93導入前から待機していたcontrol `40048d38323112fbbae2309a1eeeeeb1b6ff5637`、judge手順3によるものだった。CI詳細追加後の手順4がこのマージを実行したとは扱わない。手順4の実装は135件の制御テストと両OSのCI、最新SHAのCodex reviewを通過してmainへ導入した。

## 保存した証拠

[受領記録・判定書・probe結果](2026-09-09-production/)と、[回復世代の画像・結果・ログ](2026-09-09-production/recovered-pass-evidence/)を保持する。GitHub artifactの保持期限後も対象SHA・手順・結果を追跡するための履歴であり、期限切れの証拠を将来のマージ承認に使わない。

## 検証の限界

対象は記載したSHAと手順、世代だけであり、古いreceiptを新しいheadの承認に再利用しない。focused smokeはエディタ全体の包括的GUI検証ではない。

## Final fixからの文書修正

同じ本番final run `34291844354` は、PR #60の `c878cec39731af036f537506138f34a8828bb053` に対して文書の再レビュー説明と未解決threadを理由に `fix requested` を記録した。final-fix bridgeの後、[Claude worker 34292199132](https://github.com/hide212131/hane/actions/runs/34292199132) が手動retryなしで文書を修正し、`f562e17db741ebf1083186701ac2852c7717e59a` をpushした。両runのcontrolは `6aa424faacacb18996375da70535de07a2fe5d63`。

新SHAのCodex reviewはclean、GUI分類は不要となった。[trusted CI 34292308640](https://github.com/hide212131/hane/actions/runs/34292308640) では両OSのRustチェックは成功したが、古いbranchに `.github/tests` がなく制御テストが失敗し、世代全体はfailureを維持した。新final-fix経路の文書修正とfresh review/CIの起動までの証拠であり、#60の修正ループ完了やGUI故障を起点とするアプリコード修正の実証とは扱わない。
