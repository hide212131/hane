# Local GUI validation

## 位置づけ

GUI validation は AADW v2 の stage や judge ではない。ChatGPT Commander が Issue の acceptance criteria と変更内容から必要だと判断した場合に使う観測手段である。

全体の判断ルールは [AADW Commander Policy](aadw-command-policy.md)、採用理由と trust boundary は [ADR-0024](adr/0024-local-gui-validation.md) を参照する。

## 原則

GUI validation は focused scenario を基本とする。毎回 full regression を要求しない。

検証 evidence は current PR head と、そのシナリオに影響する current target branch / base context に対応していることを確認する。head が変わったら旧 head の GUI evidence は履歴になる。head が同じでも base の product code が変わり、scenario の結果へ影響し得る場合は、旧 evidence を current state の成功根拠として無条件に再利用しない。

GUI validator は次を行う。

- 対象 head、検証シナリオ、検証した base / merge context を evidence から確認できるようにする。
- 実アプリを操作し、観測した結果と必要な artifact を残す。
- 保存内容、状態変化、画面上の結果など、そのシナリオで確認すべき事実を確認する。
- product code を変更しない。
- 検証後の修正、再レビュー、merge など次の action を決めない。

`pass` / `fail` / `blocked` のような runner 側の結果がある場合も、それ自体を AADW の persistent state や最終判断にしない。ChatGPT が current facts と Commander Policy に基づき、product regression、acceptance blocker、validation infrastructure problem、pre-existing independent issue、unknown を区別する。

## 現在使える実行手段

Computer Use を必須条件にしない。

現在 repository に残っている明示的な GUI validation の実行入口は、ローカル Mac で起動・撮影・終了を行う `scripts/gui_validate.py` である。使い方、結果 JSON、終了コードは [CLI 操作説明](local-gui-validate-cli.md) を参照する。このコマンドは launch → ready → window → screenshot → teardown の経路を確認するもので、すべての GUI acceptance criteria を自動で検証するものではない。

このローカル CLI は指定された commit の snapshot を独立 checkout して検証し、現在の result schema は target head SHA を記録するが、PR の current base SHA や synthetic merge SHA は記録・構築しない。そのため、**base の product code が scenario に影響する PR context を、この CLI の head-only result だけで証明することはできない**。

base-sensitive な GUI acceptance では、次のいずれかを満たす evidence が必要である。

- 検証対象 head 自体が current base を取り込んでおり、そのことを GitHub facts で確認できる。
- current head + current base から作った merge context を実際に検証し、その base / merge SHA を evidence に残せる runner を使う。
- scenario が base の変更に影響されないと Commander が具体的な evidence から判断できる。

これらを確認できない場合、ローカル CLI が `pass` でも base-sensitive acceptance は `unknown` のままとし、merge 方向へ進めない。CLI の result schema に実際には検証していない base SHA を単なる metadata として追加し、merge-context 検証済みと見せることもしない。

AADW v1 では GitHub-hosted macOS を使う workflow も存在し、build / launch / window discovery / capture / cleanup の経路を検証していた。しかし PR #150 でその workflow は停止・削除されており、現行 v2 には手動 dispatch できる hosted GUI validation の入口はない。そのため、hosted runner を現在使える手段として扱わない。

Phase 3 の実運用で hosted entrypoint の必要性が確認されたため、再導入は Issue #162 で独立して扱う。v1 の orchestration や judge を復活させず、current head / base context と focused scenario を明示できる観測入口だけを検討する。

現在使える手段で acceptance に必要な GUI evidence を取得できない場合は、その不足を product failure と推測しない。一方で必要な evidence が `unknown` のまま merge 方向へ進めない。

ローカルで検証する場合も、検証対象のコードを実行する環境へ不要な書き込み認証情報を渡さない。same-repository の trusted な対象を基本とし、未確認の外部 PR を個人用 Mac で無条件実行しない。

## Evidence

GitHub が自然に持つ workflow run / artifact / PR 情報がある場合はそのまま使い、AADW 専用の snapshot state や generic receipt へ複製しない。

GitHub に自然な置き場所がない画像などだけ、必要最小限の artifact として保存する。evidence には対象 head SHA、シナリオ、観測結果、artifact に加え、scenario に影響する場合は検証した base / merge context の対応が分かる情報を残す。

既存の `gui_validate.py` の head-only result については、GitHub 上の current base facts と合わせて Commander が利用可能範囲を判断する。result 自体に base / merge SHA が無い場合、その欠落を推測で補わない。

検証 runner が起動しない、artifact を取得できないなど provider / infrastructure 理由で evidence が不足する場合は product failure と推測しない。一方で必要な evidence を取得できない `unknown` のまま merge 方向へ進めない。

## v1 からの変更

AADW v1 では GUI requirement、GUI validation、Copilot judge、merge gate を workflow の状態遷移として接続していた。v2 ではその orchestration を使わず、GUI validation を必要なときだけ選ぶ独立した観測 action とする。

v1 の全文は [`docs/history/aadw-v1/local-gui-validation.md`](history/aadw-v1/local-gui-validation.md) に保存する。
