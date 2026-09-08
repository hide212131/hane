# `scripts/gui_validate.py` — 起動・撮影・終了の最小検証コマンド

[Local GUI validation 設計](local-gui-validation.md) の段階 1（「6. 検証コマンドの責務」「7. GitHub との受け渡し」）の最初の実装として、信頼するローカル checkout から Hane を起動し、対象ウィンドウを撮影して、終了処理までの証拠を残す。既存の [`scripts/capture.sh`](../scripts/capture.sh) と [`scripts/window_id.swift`](../scripts/window_id.swift) を出発点にしており、両スクリプトの既存の動作（`測定` `docs/refactor-*` からの利用を含む）は変更していない。

この文書はこのコマンド固有の運用手順（実行方法・結果形式・終了コード・制約）を記す。設計判断そのものは [ADR-0024](adr/0024-local-gui-validation.md) と設計書を正本とする。

## この文書の位置づけ

**このコマンドの追加・成功実行は、段階 1 が実機で実証済みであることを意味しない。** 実行方法と結果形式を定義するものであり、対象 Mac での実行結果は別途記録する。このコマンド自身の単体テスト（後述）は実画面・実ビルドなしで状態遷移を検証するものであり、Mac 上での実証の代わりにはならない。

## 何を確認し、何を確認しないか

このコマンドが確認するのは、次の経路（`verification_kind: "launch_and_capture_path"`）だけである。

1. 要求された commit と実際の checkout が一致し、作業コピーに変更が混入していないこと
2. Hane がビルドでき、起動し、`hane_ready` ログを出すこと
3. その実行のプロセスに属するウィンドウが見つかること
4. そのウィンドウを撮影できること
5. 検証で起動したプロセスだけを終了できること

**撮影に成功したことは、描画内容・入力・保存を含む包括的な GUI 検証の合格を意味しない。** 結果 JSON の `scope_note` に同じ注記を機械可読な形でも残す。入力・保存内容の検査、日本語入力、スクロール、AI による画像確認は別の Issue で扱う。

## 実行方法

```sh
python3 scripts/gui_validate.py [editor|cursor-boundary|cursor-scroll]
```

引数省略時は `editor` を使う。シナリオの意味は `capture.sh` と同じ:

- `editor`: 起動完了ログ `hane_ready` を出す `timing-probe` feature でビルドし、実行専用の `editor.md` を開く（`HANE_CAPTURE_FIXTURE` の指定時はその文書を直接開き、相対画像などの参照元ディレクトリを維持する）。通常ビルドはこのログを出さない。合成入力を行う `instrument` は使わない。文書のパスを渡すことで、初回の既定フォルダ選択ダイアログを撮影することを防ぐ。このシナリオは起動・撮影のみで、文書の編集操作を行わない。将来の入力・保存シナリオには、指定された元文書を流用せず、専用の一時文書を使う。指定文書が存在しない、ディレクトリである、読み取り権限がない、UTF-8文書として読めない場合は、設定エラー（終了コード3）を表示し、アプリを起動しない。
- `cursor-boundary` / `cursor-scroll`: `instrument` feature でビルドし、専用の固定文書を実行ごとに生成する

macOS の `swift` / `screencapture` に依存するため、実際の起動・撮影・撮影経路の実証は macOS でのみ行える。macOS 以外では `missing_tools` により `preflight` 工程で `blocked` になる（後述）。

### 環境変数

`capture.sh` と同じ名前は同じ意味で流用する。

| 変数 | 既定値 | 用途 |
| --- | --- | --- |
| `HANE_GUI_VALIDATE_EXPECTED_SHA` | なし | 指定すると `git rev-parse HEAD` と一致しない場合に `blocked` にする |
| `HANE_GUI_VALIDATE_REQUEST_ID` | `<UTC timestamp>-<pid>` | 依頼 ID。実行ごとの保存先ディレクトリ名にもなる |
| `HANE_GUI_VALIDATE_GENERATION` | `1` | 実行世代。再配送・再実行の識別に使う |
| `HANE_GUI_VALIDATE_RUN_DIR` | `target/gui-validate/<request-id>/<generation>` | 証拠一式の保存先。checkout内はGitの無視対象に限る。非無視パスは証拠作成前にusage errorとして拒否 |
| `HANE_GUI_VALIDATE_STARTUP_TIMEOUT_SECS` | `15` | `hane_ready` を待つ上限秒数 |
| `HANE_GUI_VALIDATE_WINDOW_TIMEOUT_SECS` | `5` | 対象ウィンドウを待つ上限秒数 |
| `HANE_GUI_VALIDATE_WINDOW_ID_CMD` | なし（既定は `swift window_id.swift`） | ウィンドウ確認コマンドの差し替え。自動テストで実画面なしに注入する用途 |
| `HANE_GUI_VALIDATE_CAPTURE_CMD` | なし（既定は `screencapture -x -l`） | 撮影コマンドの差し替え。同上 |
| `HANE_GUI_VALIDATE_CAPTURE_TIMEOUT_SECS` | `15` | 撮影コマンドの完了を待つ上限秒数。超過すると `blocked` として扱う |
| `HANE_CAPTURE_FIXTURE` | なし | editorで開く既存文書 |
| `HANE_CAPTURE_CURSOR_OFFSET` | `11` | cursor-boundaryの文字位置。固定文書内の整数 `0`〜`23` |
| `HANE_CAPTURE_CURSOR_DOWN` | `32` | cursor-scrollの移動回数。固定文書に応じた整数 `0`〜`40` |

`HANE_STATE_DIR` は毎回 `<run-dir>/state` に固定され、呼び出し側からは上書きできない。普段の設定・Recent Files を使わないためである。

起動するアプリには、このシナリオで検証した設定と専用の `HANE_STATE_DIR` だけを渡す。親環境の `HANE_MEASUREMENT_EMPTY`、`HANE_NO_FOCUS`、`HANE_METRICS_CSV` などの計測設定は引き継がない。

## 結果の保存先と形式

実行ごとに `target/gui-validate/<request-id>/<generation>/` を新規作成し、削除しない（失敗時も証拠を残す）。

```text
target/gui-validate/<request-id>/<generation>/
  state/           # HANE_STATE_DIR（実行専用の設定・Recent Files）
  hane.log         # 起動したプロセスの stderr
  <scenario>.png   # 撮影できた場合のみ
  result.json      # 版付きの機械可読な結果
  summary.md       # 人が読む短い概要（result.json の summary と同一）
```

`result.json` の主なフィールド（`schema_version: 1`, `procedure_version: "gui-validate/1"`）:

- `overall_result`: `pass` / `fail` / `blocked`
- `overall_reason`: 各工程の失敗理由を結合した短い説明
- `target`: 要求 SHA・実際の SHA・一致可否・作業コピーの汚染有無
- `build`: profile・feature flags・`rustc`/`cargo` バージョン・バイナリパスと SHA-256
- `steps`: `preflight` → `build` → `launch` → `window_discovery` → `capture` → `cleanup` の各工程の `pass` / `fail` / `blocked` / `skipped` と理由
- `evidence`: ログ・画像・状態ディレクトリ・固定文書への参照
- `scope_note`: 経路確認であって包括的検証ではないことの注記

### `pass` / `fail` / `blocked` の決め方

設計書「7. GitHub との受け渡し」の表と同じ優先順位を実装している。実施した工程の結果に `fail` が一つでもあれば全体は `fail`、`fail` がなく `blocked` が一つでもあれば全体は `blocked`、すべて `pass` なら全体も `pass`。後片付け（`cleanup`）が失敗した場合も同じ表に従うため、他の工程がすべて成功していても全体は `blocked` になる。後片付けの失敗を成功として扱わないためである。

工程ごとの分類方針:

- 要求 SHA 不一致・作業コピーの汚染・必要なツール（`cargo` / `swift` / `screencapture`）の欠如は `blocked`（環境・権限の問題）
- ビルド失敗は `fail`（対象コードの問題として扱う）
- `hane_ready` 前にプロセスが終了した場合は `fail`（クラッシュ）、生きたままタイムアウトした場合は `blocked`（原因を断定しない）
- ウィンドウ確認のタイムアウトと撮影コマンドの失敗は `blocked`
- 前段が失敗・停止した工程は `skipped`（未実施を `blocked`/`fail` と区別する）

## 終了コード

| コード | 意味 |
| --- | --- |
| `0` | `overall_result: pass` |
| `1` | `overall_result: fail` |
| `2` | `overall_result: blocked` |
| `3` | 引数エラー（未知のシナリオ等） |

## 終了処理と割り込み

検証で起動したプロセスだけを `terminate` → `wait` → 必要なら `kill` の順で終了する。他の Hane プロセスや無関係なファイルを名前・パターンで一括終了・削除することはない。`SIGTERM` / `SIGHUP` を受けた場合も同じプロセスだけを終了し、`result.json` に中断理由を残したうえで `blocked` として終了する。

## 自動テスト

`scripts/tests/test_gui_validate.py`（標準ライブラリの `unittest` のみ、追加依存なし）は、ビルド・GUI操作をフェイクに置き換える。コピーや無視設定の検証には一時Gitリポジトリを、ロック継承の検証には短命のPython子プロセスを使う。実画面・実ビルドを使わずに次を確認する。

- 全工程が成功したときに `pass` になり、証拠参照が結果に残ること
- 起動タイムアウト・起動中のクラッシュ・ウィンドウ確認タイムアウト・撮影失敗のそれぞれが仕様どおり `blocked` / `fail` に分類され、後続工程が `skipped` になること
- 後片付けが失敗した場合に全体が `blocked` になり、成功として扱われないこと
- 割り込み（`Aborted`）が起きても、起動済みのプロセスだけが終了処理の対象になること
- 要求 SHA 不一致・作業コピーの汚染・必須ツール欠如がビルドより前に `blocked` として止まり、ビルドが呼ばれないこと
- 結果 JSON が契約どおりのフィールドを持つこと

実行方法:

```sh
python3 -m unittest discover -s scripts/tests
```

このテストの成功は状態遷移とデータ契約の検証であり、対象 Mac 上での起動・撮影・終了の実証を意味しない。実機での実証は別途、対象 Mac 上で本コマンドを実行し、生成された `result.json` と画像を確認して記録する。

実行用ディレクトリは未作成または空の場所を指定する。過去の証拠や既存文書がある場所は設定エラーとし、上書きしない。checkout内ではGitで無視される場所（既定は `target/` 配下）に限る。非無視パスは空でも設定エラーとなる。checkout外の専用ディレクトリも指定できる。これにより、保持した証拠が次回のclean確認を妨げない。

ビルドは対象コミットの独立した一時checkoutを作業ディレクトリとして `cargo build --locked` を実行し、記録したSHAにない依存解決へ更新しない。clean確認後、実行ディレクトリ内の所有マーカーを排他的に作成してから証拠を書き込む。同一request/generationの重複起動は一方だけが所有者となり、他方は既存の結果を上書きせずblockedで終了する。

同じログインユーザーのGUI検証全体は、OSの排他ファイルロックで直列化する。request/generationや証拠ディレクトリが異なる場合も、別の検証が使用中ならビルド・起動を行わずblockedとなる。起動したアプリにもロックを継承させ、終了処理の失敗やvalidatorの異常終了でアプリが残っている間は次の検証を止める。validatorとアプリが終了するとOSがロックを解放し、過去のロックファイルの存在だけでは次の実行を妨げない。

ビルド後にもHEADとclean状態を再確認し、preflight時から変更されていれば起動せずblockedにする。実行用fixtureの作成はこの確認後に行う。終了処理中のSIGINT/SIGTERM/SIGHUPは記録して処理を継続し、対象PIDの終了と結果の保存を途中で飛ばさない。

実行ロックを取得できない側は、先行処理の未作成ディレクトリを奪わないよう、結果ディレクトリを作成・予約しない。blockedの理由を標準エラーに出力して終了コード2を返す。呼び出し側はこの場合のログを保持し、結果ファイルの欠落をpassとして扱わない。

ロックを取得した呼び出しは、preflightやビルドで停止した場合も、結果ディレクトリの予約と証拠保存が終わるまでロックを保持する。保存失敗時にも解放処理を実行するが、生存中の子プロセスが保持するロックはその終了まで残る。

実行ロックのパスはOSのアカウント情報から求めたホーム配下の `.cache/hane/gui-validation.lock` に固定し、TMPDIRやHOMEの環境変数に依存しない。子プロセスと所有マーカーの作成時は、管理情報を確定するまで中断要求の配送を遅らせる。子プロセスにSIGTERMのブロック状態を継承させない。結果JSONは一時ファイルへの書き込み完了後に置き換え、保存失敗時は部分的なJSONを正式な結果として公開せずblockedとなる。

ビルドが失敗した場合もcheckoutの再確認を省略しない。外部変更が混入していれば製品のfailではなくblockedとする。終了処理後・結果保存中の中断要求は記録し、成功結果をblockedに更新して保存する。再保存が失敗した場合は自分の世代のcanonical result.jsonを削除する。削除自体にも失敗した場合はその理由を標準エラーへ記録し、終了コード2を返す。この場合はファイルだけで成功と判断しない。

コンパイル入力は、記録したコミットを独立した一時Gitリポジトリへcheckoutして固定する。元の作業コピーやGitオブジェクトへのハードリンクは共有せず、Cargoの出力先もこの実行専用に分離する。元の作業コピーを一時的に編集して元に戻しても、ビルド入力には入らない。ビルド用コピーのSHA・clean状態を結果に記録し、アプリ終了後にこの実行が作成したコピーだけを削除する。
