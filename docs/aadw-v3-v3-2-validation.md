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
- current headのレビューで残ったfail-closed契約の不足2件を修正した。
  CLIの入力読み込みで`UnicodeDecodeError`（invalid UTF-8）が
  未捕捉だったため、request/resultどちらの読み込みでも捕捉して
  `EXIT_VIOLATION`とcontract violationメッセージを返すようにした
  （missing/unreadable fileの`EXIT_USAGE`は変更していない）。また
  `_is_finite_number`がint/floatの両方に`math.isfinite`を適用しており、
  任意精度の巨大int（例: `10**1000`）でfloat変換の`OverflowError`が
  漏れていたため、intはfloat変換せず有限として扱い、`math.isfinite`は
  floatのみに適用するよう修正した（boolは引き続き数値として拒否する）。
  巨大intのNoul/confidence/probabilityは0〜1範囲外として`ContractError`に
  なり、nested JSON value内の巨大intはJSON互換として許可されることを
  回帰テストで確認した。
- current headのCodeRabbit/Codexレビューで残ったfail-closed契約の不足を
  修正した。Python 3.11以降はint-from-string変換の桁数上限
  （`sys.set_int_max_str_digits`、既定4300）があり、`parse_json()`が
  `json.loads`の既定`parse_int`（組み込み`int()`）に依存していたため、
  nested JSON value内の5000桁級integerを含むJSON *text*で`ValueError`が
  未捕捉のまま漏れていた。桁数上限に依存せず小さいdigit chunkから算術で
  組み立てる`parse_int`を追加し、`json.loads(..., parse_int=parse_int)`で
  使うことで、既存契約（nested JSON value内の任意精度intは許可、
  Noul/confidence/probabilityのような0〜1 fieldでの巨大intは拒否）を
  変えずに桁数上限を回避した。また深くネストしたJSONで`json.loads`や
  `_is_json_value`の再帰から`RecursionError`が漏れ得たため、
  `parse_json()`と`_require_json_compatible()`の双方でfail closedに
  `ContractError`へ変換するようにした。5000桁級integerがnested state
  JSON textでは許可され0〜1 fieldでは拒否されること、深くネストしたJSON
  text・直接dict入力の両方で`RecursionError`を漏らさず`ContractError`に
  なること、既存テストが弱まっていないことを回帰テストで確認した。


## Issue #347: current context adoption gate

この Issue では、Jev の request/result 契約が正しくても current head/base/evidence が stale・unknown・missing なら回答を採用しない、ネットワーク・secret・write 権限を持たない小さい adoption gate を追加する。

実装対象は `scripts/aadw_jev_*.py`、`scripts/tests/test_aadw_jev_*.py` と必要最小限の fixture / 本記録に限定し、現行 Policy、`.github/`、認証、GUI workflow、fallback workflow は変更しない。

### 実装内容（current head、テスト未実行）

- `scripts/aadw_jev_adoption_gate.py` に `evaluate_adoption(context)` を追加した。
  ネットワーク・GitHub API・Jev API を呼ばず、外部副作用を持たない
  `AdoptionDecision(adoptable: bool, reason: str)` のみを返す decision-only な
  関数であり、action の選択・routing・merge・fallback は行わない。
- 検査する current context は `expected_head_sha` / `current_head_sha`
  （40桁hex、不一致なら blocked）、`base_sensitive` が true の場合の
  `expected_base_sha` / `current_base_sha`（同様に40桁hex必須・不一致なら
  blocked。false の場合は base の変化自体では拒否しない）、非空の
  `acceptance_evidence_id`、`ci_status` / `review_status`（`success` のみ
  採用可、fail / blocked / unknown / missing は拒否）、`gui_required` と
  `gui_status`（required なら `success` のみ、not_required なら未実施でも
  他条件次第で採用可）、`jev_status`（`success` 以外は blocked とし、
  Codex fallback や COMPLETE への読み替えを行わない）。
- Jev の request/result 本体は既存の `aadw_jev_contract.py` の
  `validate_request` / `validate_result` / `validate_pair` をそのまま再利用し、
  malformed JSON、非有限値、answer 欠落・型不一致、候補外 choice を検査する
  契約を弱めていない。
- 採用する action の Choice は `action_question_key` で指定した質問の
  `choice` type を要求し、選択された label が現在の
  `current_allowed_action_labels`（空を許容しない）に含まれない場合、
  および `requested_action_labels`（Jev 依頼時点の候補集合）と
  `current_allowed_action_labels` が一致しない場合（候補集合が変化した
  stale なケース）を区別してどちらも blocked にする。
- `scripts/tests/test_aadw_jev_adoption_gate.py` に、exact head/base +
  evidence success での採用可、head/base 不一致、CI/review の
  fail・blocked・unknown、GUI required/not_required の区別、Jev failure が
  fallback や COMPLETE を意味しないことの確認、候補外 choice、候補集合の
  変化、Jev contract 違反（malformed・非有限値・answer 欠落）、不正な
  context 形状（非 dict、SHA 形式不正、pr_number 不正）を含むテストを
  追加した。このテストと gate 自体は本 worker の実行環境ではまだ実行して
  いない。CI・レビュー・GUI 検証・実サービス接続の成功は別途 Commander が
  観測する。
- CI の `Test AADW Jev contract` ステップは `.github/` 変更が禁止範囲のため
  引き続き `python3 scripts/tests/test_aadw_jev_contract.py` のみを実行する。
  このコマンドが `unittest.main()` に渡すのは自モジュール（`__main__`）の
  namespace だけで、別ファイルの `test_aadw_jev_adoption_gate.py` は自動では
  発見されないため、そのままでは adoption gate のテストが一件も実行されない
  まま CI が成功し得た。`scripts/tests/test_aadw_jev_contract.py` の
  `if __name__ == "__main__":` 側で `test_aadw_jev_adoption_gate` を import し、
  両モジュールのテストを一つの `unittest.TestSuite` にまとめて同一プロセスで
  実行してから終了コードを返すよう最小修正した。既存 contract テストは
  減らさず、`python3 scripts/tests/test_aadw_jev_adoption_gate.py` 単体実行も
  従来どおり成立する（そちらの `__main__` は変更していない）。この結合も
  本 worker の実行環境では実行していない。CI・レビュー・GUI 検証・実サービス
  接続の成功は別途 Commander が観測する。
