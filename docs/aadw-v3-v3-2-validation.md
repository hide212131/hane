# AADW v3 V3-2 検証記録

Issue #341 の固定データ契約検査と、Issue #333 の実サービス接続証拠を分けて記録する。

この初期コミットは通常workerへ依頼するための最小候補であり、Jev実装・fixture検査・実サービス接続の成功を主張しない。

## このPRで確認すること

- Choice / Noul のrequestとresponseを対応付けてfail closedで検査する。
- 不正JSON、回答欠落、型不一致、候補外、非有限値、範囲外、usage不正を拒否する。
- CIの `Test AADW Jev contract` がcurrent headで実行される。
- fixture成功を実サービス接続成功へ読み替えない。

## 実装内容（Issue #341）

- `scripts/aadw_jev_contract.py` に、2026-09-26確認の公式JavaScript SDK
  （`@typesafe-ai/sdk` 0.6.0）契約に基づく固定データ検査器を追加した。
  標準ライブラリのみを使い、Jev API・SDKへのネットワーク呼び出しは行わない。
- 検査対象は System One の request（`state` / 非空の `questions` / 任意の
  `model`）と result（非空の `model` / `questions` の key に対応する
  `answers` / `usage`）の対応関係。Noul answer は `noul` が有限の0〜1、
  Choice answer は `choice` が文字列かつrequestのcriteria候補内、`confidence` と
  `probabilities` の各値が有限の0〜1、`probabilities` のキー集合がcriteria
  のキー集合と一致することを検査する。`choice` がlist/dictなどの
  非文字列・unhashable値の場合もdict membership判定前に文字列判定を行い、
  `TypeError` を漏らさず `ContractError` として拒否する。`usage.input_tokens` /
  `output_tokens` は非負整数（bool値は整数として受理しない）を要求する。
  Score はこの検査対象に含めない。
- `request.state`、`questions[key].instructions`（存在する場合）、Choice
  question の `criteria`（label -> 値、必須）、Noul question の `criteria`
  （`null` または `true`/`false` の値、存在すれば任意）は、公式SDKの
  `EntryType`（`string | JSON object | JSON array | null`）として検査する。
  `EntryType` のトップレベル値としては number / bool を許可しないが、
  object・array内部のJSON互換値（number/bool を含む）は許可する。
  NaN/Infinity/-Infinity はJSON文字列経由・直接dict入力のどちらでも、
  ネストの深さに関わらずfail closedで拒否する。この拒否はrequest/result
  全体をJSON互換値として検査することで行うため、上記の既知fieldに限らず、
  スキーマ上未走査の追加fieldに含まれる非有限値も同様に拒否する。
- `scripts/tests/test_aadw_jev_contract.py` で、有効なChoice+Noulの組と、
  上記の必須失敗条件（不正JSON、questions空、answer欠落、型不一致、
  Noul/Choiceの範囲外・非有限値、Choiceの候補外・キー欠落/余分、
  Choiceがlist/dictなどのunhashable値の場合、model空、
  usage不正）を確認する。unhashableなChoiceのケースはCLI経路でも
  tracebackではなく非0の契約違反終了になることを確認する。
  JSON parserが非標準のNaN/Infinity相当を受理し得る
  点を踏まえ、数値の有限性を明示的に検査するケースを含む。`instructions` /
  `criteria` が省略・`null` でも有効な回帰と、number/bool instructions、
  number state、不正なcriteria description、ネスト内非有限値など
  `EntryType` 型不一致の拒否ケースも含む。
- この検査はfixtureレベルの契約検査であり、実サービスへの接続確認では
  ない。実サービス接続の証拠はIssue #333側で別途記録する。
