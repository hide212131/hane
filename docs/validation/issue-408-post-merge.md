# Issue #408 導入後の独立検証

この文書は、PR #409 の main 導入後確認を既存経路で実行するための検証対象を記録します。エディタ本体、測定器、workflow、Policy は変更しません。

## 基準と目的

- 作成時の main: `263a00f7f058de7949710317a10f37322d372efd`
- 対象: Issue [#408](https://github.com/hide212131/hane/issues/408) の変更なし診断と、実環境での画面観測可否
- このPRは実行経路の対象であり、スクロール機能の受入判定や #395 の停止解除を意味しません。

## 実行契約

1. main の `AADW Diagnose` に対し、このPRのcurrent headを対象に診断を一度だけ依頼します。診断結果のartifact、変更なし検査、対象SHAを照合し、診断完了と製品受入を区別します。
2. 承認済みの `AADW Hosted GUI Validation` 経路から `scroll-inertia` の `merge` contextを一度実行し、trusted procedure `hosted-scroll-inertia/8` と実行対象のhead/baseを記録します。
3. 元の判定、`observation_quality`、撮影記録を比較します。Lines/Pixels の80ms判定、反転の既存55ms条件、producer側135msの撮影窓はそれぞれ現行mainの実装どおりに扱い、互いに読み替えません。
4. GUI結果は測定が成立した範囲と測定不能の範囲を分けて記録します。unknown・blocked・failを製品合格へ書き換えません。

## 用途終了

検証結果をIssue #408へ記録した後、この文書だけのPRはマージせず閉じます。
