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
| `HANE_GUI_VALIDATE_RUN_DIR` | `target/gui-validate/<request-id>/<generation>` | 証拠一式（状態・ログ・画像・結果）の保存先 |
| `HANE_GUI_VALIDATE_STARTUP_TIMEOUT_SECS` | `15` | `hane_ready` を待つ上限秒数 |
| `HANE_GUI_VALIDATE_WINDOW_TIMEOUT_SECS` | `5` | 対象ウィンドウを待つ上限秒数 |
| `HANE_GUI_VALIDATE_WINDOW_ID_CMD` | なし（既定は `swift window_id.swift`） | ウィンドウ確認コマンドの差し替え。自動テストで実画面なしに注入する用途 |
| `HANE_GUI_VALIDATE_CAPTURE_CMD` | なし（既定は `screencapture -x -l`） | 撮影コマンドの差し替え。同上 |
| `HANE_GUI_VALIDATE_CAPTURE_TIMEOUT_SECS` | `15` | 撮影コマンドの完了を待つ上限秒数。超過すると `blocked` として扱う |
| `HANE_CAPTURE_FIXTURE` / `HANE_CAPTURE_CURSOR_OFFSET` / `HANE_CAPTURE_CURSOR_DOWN` | `capture.sh` と同じ | シナリオ別の入力・オフセット調整 |

`HANE_STATE_DIR` は毎回 `<run-dir>/state` に固定され、呼び出し側からは上書きできない。普段の設定・Recent Files を使わないためである。

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

`scripts/tests/test_gui_validate.py`（標準ライブラリの `unittest` のみ、追加依存なし）は、`git` / `cargo` / `swift` / `screencapture` / 実際の Hane プロセスをすべて差し替え可能なフェイクに置き換えて実行する。実画面・実ビルドを使わずに次を確認する。

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

実行用ディレクトリは未作成または空の場所を指定する。過去の証拠や既存文書がある場所は設定エラーとし、上書きしない。作業コピーのclean確認は、検証用文書・設定の作成より前に行うため、checkout内のgitignore対象外の空ディレクトリも利用できる。
