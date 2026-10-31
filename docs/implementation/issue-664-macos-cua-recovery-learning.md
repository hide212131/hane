# Issue #664: macOS `noWindowsAvailable`の復旧観測

## 目的と状態

Issue [#664](https://github.com/hide212131/hane/issues/664)に基づき、2026-10-31のmacOS GUI復旧観測を`docs/gui-test-recovery.md`へ追加するための文書作業用Draft PR。この文書は準備時点の記録であり、準備commitを作った時点では復旧ガイドへの変更は未実施だった。その後、同じPRのhead `352dedfe698a2c215430c28974cb8c0217750fa2`で`docs/gui-test-recovery.md`への追記は実装済みである（この状態表記を修正する前のheadとして記録する）。以下の「Claudeに依頼する範囲」以降は準備時点の依頼内容であり、現在の実装状態や実装完了の証拠を示すものではない。この追記は新たなGUI成功、原因特定、PR #638のG4合格を意味せず、G4は`unknown`のままである。

対象時点のmain: `09deafcc3085d0b73675c32b74aee624a1c12579`
関連するGUI検証: PR #638、head `c44d4f79464e1be729b8a5f5c8c94b43f87fc6c6`

## Claudeに依頼する範囲

`docs/gui-test-recovery.md`だけを更新し、現在のmacOS `noWindowsAvailable`節に、今回の日付付き観測を追加する。既存のPR #507で確認したFinder通常Openの記録と矛盾させず、今回の事実は追加観測として記す。

## 必ず保つ事実の区別

- ChatGPTアプリを再起動しても、Calculatorの座標クリックは`noWindowsAvailable`のままだった。
- 実行パスを照合した`SkyComputerUseService`を再起動してCUAを再接続しただけでも、Calculatorの座標クリックは戻らなかった。
- FinderからCalculator.appを通常の「開く」で起動し直した後、Calculatorの座標クリックが成功した。
- Finderから隔離したPR #638候補を通常の「開く」で起動し直した後、Hane本文への無害な座標クリックが1回成功した。
- Hane候補では後の終了操作と再取得がタイムアウトした。したがってGUI全体の復旧や元の受入シナリオの完了とは言えない。
- PR #638のG4は`unknown`/未完了のまま。軽量クリック成功で合格にしない。
- 観測は復旧操作の有効性を示すだけであり、内部原因や常に成功する保証を確定しない。
- 専用Haneプロセスを照合して終了し、共有ロック解放と専用state/fixture復元を確認済み。

一次記録はこの作業開始時点のローカル実行記録`/private/tmp/pr638-g4-retry-20261031-g_ysv6u1/mac-g4-result.json`とその同ディレクトリ内の証拠である。ローカル一時パスは公開文書に恒久参照として載せず、PR上で共有可能な証拠が追加されない限り、公開済みの証拠リンクがあるとは記載しない。

## 対象外

- Hane製品コード、CUA/helper、workflow、テストや自動化機構の変更
- PR #638の差分・受入判断・Draft状態・レビュー・merge
- 原因の断定、ChatGPT/helper再起動の一般的な処方化
- 他OSの復旧方法

## 受け入れ条件

- 復旧ガイドに、日付・症状・比較した対処・成功した軽量操作・残った失敗を簡潔に記録する。
- 「Finder通常Open後の限定的なクリック成功」と「Haneの終了・再取得失敗」を分ける。
- G4を未完了/unknownとし、既存のcurrent PR受入要件を免除しない。
- 変更は上記の文書だけ。準備文書は実装完了の証拠ではない。
