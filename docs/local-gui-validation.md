# Local GUI validation

## 位置づけ

GUI validation は AADW v2 の stage や judge ではない。ChatGPT Commander が Issue の acceptance criteria と変更内容から必要だと判断した場合に使う観測手段である。

全体の判断ルールは [AADW Commander Policy](aadw-command-policy.md)、採用理由と trust boundary は [ADR-0024](adr/0024-local-gui-validation.md) を参照する。

## 原則

GUI validation は focused scenario を基本とする。毎回 full regression を要求しない。

検証 evidence は current PR head と、そのシナリオに影響する current target branch / base context に対応していることを確認する。head が変わったら旧 head の GUI evidence は履歴になる。head が同じでも base の product code が変わり、scenario の結果へ影響し得る場合は、旧 evidence を current state の成功根拠として無条件に再利用しない。

GUI validator は次を行う。

- 対象 head と検証シナリオを明示する。base-sensitive な scenario を検証する場合は、実際に検証した base / merge context も evidence から確認できるようにする。
- 実アプリを操作し、観測した結果と必要な artifact を残す。
- 保存内容、状態変化、画面上の結果など、そのシナリオで確認すべき事実を確認する。
- product code を変更しない。
- 検証後の修正、再レビュー、merge など次の action を決めない。

base の変更が scenario に影響しないと扱うかどうかは GUI worker が決めない。ChatGPT Commander が current facts と scenario の性質から判断し、その根拠を GitHub 上の Issue / PR など自然な場所に残す。

`pass` / `fail` / `blocked` のような runner 側の結果がある場合も、それ自体を AADW の persistent state や最終判断にしない。ChatGPT が current facts と Commander Policy に基づき、product regression、acceptance blocker、validation infrastructure problem、pre-existing independent issue、unknown を区別する。

## 現在使える実行手段

Computer Use を必須条件にしない。現在は GitHub-hosted macOS の Hosted GUI Validation と、ローカル Mac の CLI を用途に応じて使える。

### Hosted GUI Validation

通常は対象 Pull Request の Conversation に、次のどちらかを単独のコメントとして投稿する。

```text
/gui-validate head
```

```text
/gui-validate merge
```

Issue #126 の通常リスト表示だけを確認する場合は、専用のコマンドを使う。

```text
/gui-validate normal-list head
```

```text
/gui-validate normal-list merge
```

このコマンドは `scripts/hosted_normal_list_gui.py` の信頼済み手順へ振り分けられる。総合の GUI 検証結果や日付表示専用の検証結果を代用するものではない。

`head` は exact current PR head を検証する。`merge` は current target branch と current PR head から GitHub が作る current PR merge ref を検証する。文章中にコマンド文字列を書いた場合や、引数がない・未知の引数を付けた場合は起動しない。

comment router は trusted default branch の workflow / script だけを使い、コメントイベントから PR 番号を取得する。GitHub API から current PR metadata を取得し、コメント投稿者が `write` / `maintain` / `admin` のいずれかであること、PR が open かつ non-draft であること、same-repository PR であることを確認する。head SHA と current target branch SHA は人に入力させず、その時点の GitHub facts から解決する。

procedure version は trusted default branch の `scripts/hosted_gui_interaction.py` にある `PROCEDURE_VERSION` を読み、人に転記させない。現在の総合 procedure は `hosted-gui-interaction/7` である。

router が作る依頼は PR、exact head、router が観測した current base、execution context、procedure に結び付ける。同じ context の queued / in-progress / success run がすでにある場合は重複起動を抑止する。base SHA も識別に含めるため、head が同じまま target branch が進んだ場合に、古い base の成功 run を current evidence として自動再利用しない。

Hosted GUI Validation 側でも router の値をそのまま信用しない。実行時に元コメント、投稿者権限、PR の open/non-draft、same-repository、exact head、current base、trusted procedure を再取得・再検証する。router の依頼後に head または base が動いた場合は fail closed とし、新しい context へ暗黙に置き換えない。`merge` では current PR merge ref が2 parent の merge commit であり、第1 parent が検証時の current base、第2 parent が requested exact head であることも確認する。

受理した依頼は Pull Request Conversation に head / base / execution context / procedure / workflow run URL を通知する。この通知コメント自体は trust evidence ではない。workflow run、artifact、current PR facts を evidence として使う。

障害時の fallback として Actions の `AADW Hosted GUI Validation` から manual `workflow_dispatch` も残す。manual fallback でも trusted actor、open/non-draft、same-repository、exact-head、current-base、merge-ref parent、procedure の検証は省略しない。

Hosted run の artifact には `aadw-context.json` と validator が生成した公開可能な screenshot / structured observation / log を保存する。`aadw-context.json` から requested head、requested/observed base、execution context / SHA、trusted control SHA、procedure、workflow run を対応付けられる。

### ローカル CLI

ローカル Mac では `scripts/gui_validate.py` で launch → ready → window → screenshot → teardown を確認できる。使い方、結果 JSON、終了コードは [CLI 操作説明](local-gui-validate-cli.md) を参照する。このコマンドが確認する範囲を超える acceptance criteria は、別の focused scenario と evidence が必要である。

このローカル CLI は指定された commit の snapshot を独立 checkout して検証し、現在の result schema は target head SHA を記録するが、PR の current base SHA や synthetic merge SHA は記録・構築しない。そのため、**base の product code が scenario に影響する PR context を、この CLI の head-only result だけで証明することはできない**。

base-sensitive な GUI acceptance では、次のいずれかを満たす evidence が必要である。

- 検証対象 head 自体が current base を取り込んでおり、そのことを GitHub facts で確認できる。
- current head + current base から作った merge context を実際に検証し、その base / merge SHA を evidence に残せる Hosted GUI Validation を使う。
- scenario が base の変更に影響されないと Commander が具体的な evidence から判断できる。

これらを確認できない場合、ローカル CLI が `pass` でも base-sensitive acceptance は `unknown` のままとし、merge 方向へ進めない。CLI の result schema に実際には検証していない base SHA を単なる metadata として追加し、merge-context 検証済みと見せることもしない。

現在使える手段で acceptance に必要な GUI evidence を取得できない場合は、その不足を product failure と推測しない。一方で必要な evidence が `unknown` のまま merge 方向へ進めない。

ローカルで検証する場合も、検証対象のコードを実行する環境へ不要な書き込み認証情報を渡さない。same-repository の trusted な対象を基本とし、未確認の外部 PR を個人用 Mac で無条件実行しない。

## Evidence

GitHub が自然に持つ workflow run / artifact / PR 情報がある場合はそのまま使い、AADW 専用の snapshot state や generic receipt へ複製しない。

GitHub に自然な置き場所がない画像などだけ、必要最小限の artifact として保存する。evidence には対象 head SHA、シナリオ、観測結果、artifact に加え、scenario に影響する場合は検証した base / merge context の対応が分かる情報を残す。

既存の `gui_validate.py` の head-only result については、GitHub 上の current base facts と合わせて Commander が利用可能範囲を判断する。result 自体に base / merge SHA が無い場合、その欠落を推測で補わない。

検証 runner が起動しない、artifact を取得できないなど provider / infrastructure 理由で evidence が不足する場合は product failure と推測しない。一方で必要な evidence を取得できない `unknown` のまま merge 方向へ進めない。

## v1 からの変更

AADW v1 では GUI requirement、GUI validation、Copilot judge、merge gate を workflow の状態遷移として接続していた。v2 ではその orchestration を使わず、GUI validation を必要なときだけ選ぶ独立した観測 action とする。

Hosted GUI Validation も v1 の state machine を復活させるものではない。comment router は authorization と current request construction、GUI workflow は execution-time validation と観測に限定する。結果から fix / review / merge を自動連鎖させない。

v1 の全文は [`docs/history/aadw-v1/local-gui-validation.md`](history/aadw-v1/local-gui-validation.md) に保存する。
