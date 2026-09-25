# AADW v3 V3-2 検証記録

Issue #341 の固定データ契約検査と、Issue #333 の実サービス接続証拠を分けて記録する。

この初期コミットは通常workerへ依頼するための最小候補であり、Jev実装・fixture検査・実サービス接続の成功を主張しない。

## このPRで確認すること

- Choice / Noul のrequestとresponseを対応付けてfail closedで検査する。
- 不正JSON、回答欠落、型不一致、候補外、非有限値、範囲外、usage不正を拒否する。
- CIの `Test AADW Jev contract` がcurrent headで実行される。
- fixture成功を実サービス接続成功へ読み替えない。
