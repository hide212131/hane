# Hane 画面操作API 設計案

状態：設計案。未実装。調査基準：main `29ed353d1548db7d2df61ffa85085dc4457c98cc`（2026-10-31）。

本書は利用者と合意した設計案を保存する。設計用PRはこの文書だけを追加し、製品実装・回帰テスト・実OS検証の完了を意味しない。実装は別の実装用PRで行う。

## 1. 目的と基本方針

定型的なGUIテストを、画面操作APIから短い手順で実行できるようにする。

対象を画像認識で探す代わりにIDで指定する。クリック・キー入力・文字入力は既存のUI処理へ配送し、本文・設定値・フォーカス状態を直接書き換えない。

```text
テスト実行側
  ↓ ローカルAPI
対象IDを現在の画面要素へ解決
  ↓
通常のマウスイベント／キー処理／文字入力処理
  ↓
既存のHane UI
  ↓
状態取得・条件待ち・必要な画面撮影
```

主な削減対象は、画面取得、OCRによる対象探索、一操作ごとのAI判断、固定時間の待機である。削減量は未測定であり、導入後に同一シナリオで比較する。

### 現行実装の根拠

- `scripts/hosted_gui_interaction.py` / `.swift`：OSキー入力、OCRクリック、ドラッグ、保存、Undo/Redo、日本語IME、スクロールの既存手順。
- `scripts/hosted_file_tabs_gui.py` / `.swift`：設定を開く、テーマ切替、ホバー表示、パスコピーの既存手順。
- `crates/ui/src/view/settings_screen.rs`：設定カテゴリとテーマ等の既存ID、設定遷移・保存処理。
- `crates/ui/src/view/input_routing.rs`、`crates/ui/src/input.rs`、`crates/ui/src/actions.rs`：既存の入力先判定、文字入力、キー割当。
- `vendor/gpui/src/window.rs`：`dispatch_event`、`dispatch_keystroke`によるイベント配送。
- `vendor/gpui/src/platform.rs`：`PlatformInputHandler::dispatch_input`から通常の`replace_text_in_range`への配送。ただし`dispatch_input`は`pub(crate)`であり、必要な最小接続を検討する。
- `vendor/gpui/src/elements/div.rs`、`vendor/gpui/src/app/test_context.rs`：`debug_selector` / `debug_bounds`はテスト用の位置取得であり、通常配布ビルドの外部操作APIではない。

調査ではコード・テスト定義・手順書を確認した。実アプリでの実行結果や全テストの成功を主張するものではない。実装着手時にcurrent mainを読み直す。

## 2. APIが通す入力経路

| 操作 | 配送方法 |
|---|---|
| クリック | 現在の描画位置へマウス移動・押下・解放イベントを送る |
| ダブルクリック・右クリック | 対応するボタン・クリック回数のマウスイベントを送る |
| キー操作 | GPUIのキー割当・イベント伝播を通す |
| 文字入力 | 現在フォーカスされている通常の入力ハンドラーへ確定文字列を渡す（IME composition中は配送しない） |
| スクロール | 指定した位置へ通常のスクロールイベントを送る |
| ホバー・ドラッグ | マウス移動と押下・解放の系列を送る |

ボタンのコールバックを直接呼ぶ方式や、`EditorCommand`を外部から直接実行する方式にはしない。通常のクリック判定や入力先判定が働く経路を維持する。

文字列の入力とキー操作は区別する。Enter、Tab、Escape、ショートカットはキー操作として送る。

この経路はアプリ内へ入力イベントを注入するものであり、OSの入力配送や日本語IME自体を通ったことの証明にはならない。`dispatch_keystroke`にも模擬入力の処理があるため、その成功を実OSのIME成功へ読み替えない。

## 3. 要素IDと対象の取得

既存IDを優先して利用し、不足する操作対象に安定したIDを付ける。表示文言、画面座標、表示順には依存させない。

| 対象 | IDの例 | 扱い |
|---|---|---|
| ライトテーマ | `settings-theme-light` | 既存ID |
| 自動保存 | `settings-autosave` | 既存ID |
| AI設定カテゴリ | `settings-category-ai` | 既存ID |
| 本文 | `editor-body` | API向け登録案 |
| 文書内検索欄 | `document-find-query` | API向け登録案 |
| 名前変更欄 | `sidebar-inline-rename` | API向け登録案 |

既存IDがあるだけでは外部APIから位置や状態を取得できない。描画処理との対応付けを追加する。

IDはウィンドウ内で一意とする。タブ・ファイル行・検索結果など繰り返し要素は、セッションや対象項目の識別子と組み合わせる。行番号や表示名だけでは識別しない。

描画時に、API用の小さな要素台帳を更新する。

- ID、役割、表示名。
- 現在の表示領域とクリック可能な位置。
- 表示・操作可能状態。
- フォーカス、選択・チェック状態。
- 要素の世代と描画フレーム番号。

台帳には操作対象と観測用の情報だけを登録する。画面の全構造や業務状態を別管理しない。

`ui.query`などの対象取得は、IDに加えて、その時点の要素を指す一時的な参照（`ref`）と要素の世代（`generation`）を返す。ファイル名変更欄などが閉じて開き直された場合、古い参照は失効させる。

対象を操作・観測する`ui.inspect`、`ui.click`、`ui.wait`は、IDに加えて、取得した`ref`と`generation`を要求に必ず含める。dispatcherは、要求の`ref`・`generation`が台帳に登録された要素の現在の値と一致する場合だけ、その要素を対象にする。一致しない場合、失効済みの場合、再描画で別の要素に置換された場合は、同じIDの現行要素へ再解決しない。画面入力の配送も待機の成功も行わず、`target_stale`を返す。呼び出し側は`ui.query`で対象を取得し直してから再試行する。待機開始時点で存在しない対象の出現待ちだけは、対象自身の`ref`・`generation`の代わりに、安定した親対象の`ref`・`generation`を要求する（§4.1）。

## 4. 最小API

| API | 内容 |
|---|---|
| `session.info` | 対象プロセス、ビルド、ウィンドウ、対応機能を取得 |
| `ui.query` | ID・役割・親領域で対象を検索し、`ref`と`generation`を返す |
| `ui.inspect` | `id`・`ref`・`generation`で指定した対象の状態を取得 |
| `ui.click` | `id`・`ref`・`generation`で指定してクリック |
| `ui.key` | 現在の入力先へキー操作 |
| `ui.text` | 現在の入力先へ確定文字列を入力（入力先がIME composition中なら`composition_active`で失敗）。検索欄への入力が検索を開始した場合は、その検索のクエリ世代`query_generation`を結果に返す |
| `ui.wait` | 既存の対象は`id`・`ref`・`generation`で指定し、指定した状態になるまで期限付きで待機。検索完了を待つ場合は、`ui.text`が返した`query_generation`も必須。待機開始時点で存在しない対象の出現は、安定した親対象とquery条件を指定する出現待ち（`appear`、§4.1）で待つ |
| `ui.batch` | 複数操作を順番に実行 |
| `ui.capture` | 必要なときに画面を撮影 |
| `ui.request_status` | 同一セッション内の`request_id`から実行状況を読み取り専用で照会（元の操作は再実行しない。§8.1） |

`ui.click`、`ui.key`、`ui.text`、`ui.batch`は、呼び出し側が事前に生成した`request_id`を要求に必須とする（§8.1）。

`ui.key`と`ui.text`には、期待する入力先を必須指定する。実際のフォーカスと一致しなければ入力せず失敗する。

`ui.text`は、`expected_focus`の確認に加えて、対象の入力先がIME composition中かを確認する。composition中（`composition_active`）なら、文字入力イベントを配送せず、compositionのcommit・cancel・置換も行わない。本文・入力欄・Undo履歴・composition stateは変更せず、`composition_active`を返す。compositionがactiveでない場合は、従来どおり確定文字列を通常の入力ハンドラーへ渡す。

検索欄への`ui.text`が成功し、その入力で検索が開始された場合、結果に`query_generation`を含める。これは検索クエリの世代を表す不透明な値で、入力ごとに新しい値が割り当てられ、同じ検索欄で先行した検索の値とは異なる。要素の`ref`・`generation`（要素の登録・置換を表す値）とは別の概念であり、相互に代用しない。入力が検索を開始しなかった場合（例: 失敗、または検索を起動しない入力先）は`query_generation`を返さない。

検索完了を待つ`ui.wait`は、`query_generation`を必須指定する。`ui.wait`は、要求した`query_generation`と同じ検索が完了した（その世代について`pending: false`かつ結果が確定した）場合だけ成功を返す。先行検索の`pending: false`や古い結果では成功しない。要素の`ref`・`generation`の照合は従来どおり別に行い、両方が一致しなければ成功しない。相関値の不一致・失効時の結果は§8に定める。

### 4.1 まだ存在しない対象の出現待ち

ダイアログなど、`ui.wait`の開始時点で存在しない対象は`ref`・`generation`を持てない。この場合だけ、`id`・`ref`・`generation`の代わりに`appear`を指定する。`appear`と、既存対象を指す`id`・`ref`・`generation`は同時に指定できない。

- `parent`: 出現を待つ範囲を限定する、すでに存在する安定した親対象（例: ウィンドウ直下のダイアログ配置領域）。`ui.query`で取得した`id`・`ref`・`generation`を必須とする。
- `match`: 親の配下で探す対象のquery条件。`id`を必須とし、必要なら`role`を加える。表示文言、座標、表示順は使わない。
- `state`と`timeout_ms`: 出現した対象に求める状態と期限。既存の`ui.wait`と同じ意味とする。

待機anchorは、dispatcherが待機開始時に台帳から記録する`parent`の`ref`・`generation`と描画フレーム番号（`anchor_frame`）である。これは呼び出し側が指定する値ではなく、`query_generation`・`request_id`とは別の概念である。

- anchorより後（`anchor_frame`より大きいフレーム）に台帳へ登録された、`parent`配下で`match`に合う要素だけを候補にする。待機開始時にすでに存在した要素は候補にしない。開始時点で存在する対象は、`ui.query`で`ref`・`generation`を取得して従来の形式で待つ。
- 候補が最初に一つ見つかった時点で、その要素の`ref`・`generation`へ待機を結び付ける（束縛）。以降は`id`で現行要素を再解決しない。束縛後に`state`が満たされる前にその要素が閉じる・置換されると、別要素の同じ`id`で成功せず`target_stale`を返す。
- 同時に候補が複数見つかった場合は、どれも選ばず`target_ambiguous`を返す。
- 待機中に`parent`の`ref`・`generation`が登録要素と一致しなくなった場合は、親が閉じた・置換された場合も含め、`target_stale`を返す。
- 期限までに候補が現れない、または束縛した対象が`state`を満たさない場合は`timeout`を返す。応答に、`reason`（`no_match`または`state_unmet`）と`anchor_frame`を含める。他の要素や過去の状態で代用しない。

要求の例（値は新設案の例）。

```json
{
  "method": "ui.wait",
  "params": {
    "appear": {
      "parent": { "id": "dialog-host", "ref": "el-3", "generation": 1 },
      "match": { "id": "settings-dialog", "role": "dialog" }
    },
    "state": { "visible": true },
    "timeout_ms": 3000
  }
}
```

成功時は、束縛した要素の`ref`・`generation`を返す。以降の`ui.inspect`・`ui.click`・`ui.wait`には、この値を明示的に渡す。

```json
{
  "result": {
    "target": { "id": "settings-dialog", "ref": "el-41", "generation": 1 },
    "anchor_frame": 1208,
    "state": { "visible": true }
  }
}
```

期限までに対象が現れなかった場合の失敗応答の例。

```json
{
  "error": {
    "code": "timeout",
    "reason": "no_match",
    "anchor_frame": 1208
  }
}
```

出現後に対象が置換された場合の失敗応答の例。置換後の同じ`id`の要素では成功しない。

```json
{
  "error": {
    "code": "target_stale",
    "requested": { "id": "settings-dialog", "ref": "el-41", "generation": 1 }
  }
}
```

例えば、検索欄へ入力する場合は次の順序になる。

```text
検索欄をクリック
→ 検索欄にフォーカスがあることを確認
→ 文字を入力
→ 検索結果の更新を待つ
```

入力APIが対象欄へ勝手にフォーカスを移すことはしない。クリックによるフォーカス移動に不具合があれば、その段階で検出する。

第2段階で`ui.hover`、`ui.drag`、`ui.scroll`を追加する。本文の任意文字位置を指定する操作は、既存の文字位置と描画座標の変換を再利用して別途設計する。

## 5. 呼び出し例

以下の検索欄ID、結果状態名、`ref`、`generation`の値は新設案の例である。検索欄を開いた後に実行する。

まず`ui.query`で対象を取得する。応答の`ref`と`generation`を、以降の要求へ明示的に渡す。

```json
{
  "method": "ui.query",
  "params": { "window": "main", "id": "document-find-query" }
}
```

```json
{
  "result": {
    "targets": [
      { "id": "document-find-query", "ref": "el-17", "generation": 4 }
    ]
  }
}
```

```json
{
  "method": "ui.batch",
  "params": {
    "window": "main",
    "stop_on_error": true,
    "steps": [
      {
        "method": "ui.click",
        "params": { "id": "document-find-query", "ref": "el-17", "generation": 4 }
      },
      {
        "method": "ui.wait",
        "params": {
          "id": "document-find-query",
          "ref": "el-17",
          "generation": 4,
          "state": { "focused": true },
          "timeout_ms": 1000
        }
      },
      {
        "method": "ui.key",
        "params": {
          "expected_focus": "document-find-query",
          "key": "primary-a"
        }
      },
      {
        "id": "enter-query",
        "method": "ui.text",
        "params": {
          "expected_focus": "document-find-query",
          "text": "見出し"
        }
      },
      {
        "method": "ui.wait",
        "params": {
          "id": "document-find-results",
          "ref": "el-23",
          "generation": 2,
          "query_generation": { "from_step": "enter-query" },
          "state": { "pending": false },
          "timeout_ms": 3000
        }
      }
    ]
  }
}
```

`document-find-results`の`ref`（`el-23`）と`generation`も、同様に`ui.query`で取得済みの値である。`ref`・`generation`が現在の登録要素と一致しない手順は、同じIDの現行要素へ再解決せず`target_stale`で失敗し、一括実行はそこで停止する。

`query_generation`の`{ "from_step": "enter-query" }`は、同じ一括実行内で`id`が`enter-query`の手順の結果に含まれる`query_generation`を指す。その手順が成功しなかった場合や`query_generation`を返さなかった場合、この`ui.wait`は待機せず`query_generation_mismatch`で失敗する。一括実行を使わない場合は、`ui.text`の応答の値を`ui.wait`へ明示的に渡す。

`ui.text`の成功結果の例は次のとおりである（値は新設案の例）。

```json
{
  "result": {
    "delivered": "text",
    "query_generation": "q-31"
  }
}
```

同じ検索を待つ単独の`ui.wait`要求の例は次のとおりである。

```json
{
  "method": "ui.wait",
  "params": {
    "id": "document-find-results",
    "ref": "el-23",
    "generation": 2,
    "query_generation": "q-31",
    "state": { "pending": false },
    "timeout_ms": 3000
  }
}
```

成功時の応答には、完了を確認した`query_generation`（`q-31`）を含める。

```json
{
  "result": {
    "query_generation": "q-31",
    "state": { "pending": false }
  }
}
```

`q-31`の検索が完了する前に、別の入力で新しい検索（例: `q-32`）が開始された場合、`q-31`は失効する。この`ui.wait`は`q-32`の完了や`q-30`以前の結果では成功せず、次のように失敗する。

```json
{
  "error": {
    "code": "query_generation_stale",
    "requested": "q-31",
    "current": "q-32"
  }
}
```

例の`ui.text`は、検索欄がIME composition中でない場合に確定文字列を入力する。composition中なら入力せず`composition_active`で失敗し、一括実行はそこで停止する。

`primary`はmacOSではCommand、WindowsではCtrlとする。実際に配送したキーは結果に記録する。

一回の呼び出しで操作列を実行し、通常時は短い結果だけ返す。画像や詳細な状態は要求時・失敗時に取得する。

## 6. クリックの成立条件

クリック直前に最新の描画状態から対象を解決する。

- 対象が一つに決まること。
- 表示領域内に操作可能な位置があること。
- 無効化されていないこと。
- 別のダイアログやメニューに遮られていないこと。
- 要求の`ref`・`generation`が、台帳に登録された要素の現在の値と一致すること。

画面外の対象を探すためのスクロールは、明示的な別操作とする。クリックの裏で設定画面を開いたり、フォルダを展開したりしない。

通常のヒットテストを利用して遮蔽を確認する。成立を確認できない場合は、ボタン処理を直接呼んで補わず失敗する。

## 7. 操作完了と結果確認

「イベントを送れた」と「期待した結果になった」を区別する。

クリックAPIの成功は入力の配送完了を表す。テーマ変更、検索完了、保存成功などは、後続の状態確認で検証する。

待機条件は、実際の状態に対応させる。

| 条件 | 観測するもの |
|---|---|
| フォーカス移動 | 現在の入力先 |
| ボタン切替 | 選択・チェック状態 |
| 検索完了 | `ui.text`が返した`query_generation`と、その世代の処理状態 |
| 保存完了 | 対象文書・リビジョンに対応する保存結果 |
| ダイアログ表示 | 開始時に存在しない対象は、`appear`（§4.1）で指定した親対象の配下に、待機anchor以降に登録された要素の出現。出現後の要素の`ref`・`generation`へ束縛して状態を確認する |

「アプリ全体が落ち着くまで待つ」という曖昧な条件は設けない。次の描画フレームが完了しただけで、保存や検索の完了とは扱わない。呼び出し例の`pending: false`も、直前の`ui.text`が返した`query_generation`の検索についてだけ確認し、以前の検索の`pending: false`や結果で成立させない。要素の`ref`・`generation`は要素の同一性を確認する値であり、検索クエリの世代の代わりにはしない。

## 8. 状態取得と失敗時の扱い

通常の応答は、要求した対象・項目に限定する。文書全体や画面全体の要素一覧を毎回返さない。

エラーは少なくとも次を区別する。

`target_not_found`、`target_ambiguous`、`target_stale`、`target_disabled`、`target_occluded`、`focus_mismatch`、`composition_active`、`query_generation_mismatch`、`query_generation_stale`、`request_id_required`、`request_id_duplicate`、`request_limit_exceeded`、`timeout`。

`target_stale`は、`ui.inspect`、`ui.click`、`ui.wait`の要求の`ref`・`generation`が登録要素と一致しない、失効済み、または再描画で置換された場合に返す。同じIDの現行要素へ再解決せず、画面入力も待機成功も発生させない。出現待ち（§4.1）では、`parent`の不一致・失効、および出現後に束縛した要素の置換・失効でも返す。

出現待ちの`target_ambiguous`は、待機anchor以降に`match`へ合う候補が同時に複数登録された場合に返す。`timeout`は、`reason`が`no_match`（候補が現れない）または`state_unmet`（束縛した対象が期限までに`state`を満たさない）のいずれかを示し、`anchor_frame`を含める。`anchor_frame`は台帳の描画フレーム番号であり、`query_generation`・`request_id`・要素の`generation`の代わりにならない。

`composition_active`は、`ui.text`の入力先がIME composition中の場合に返す。文字入力イベントの配送、compositionのcommit・cancel・置換、本文・入力欄・Undo履歴・composition stateの変更はいずれも発生させない。

`query_generation_mismatch`は、検索完了を待つ`ui.wait`で`query_generation`が未指定、対象の検索欄が発行していない値、または参照先の手順が値を返していない場合に返す。`query_generation_stale`は、指定した`query_generation`が、より新しい入力による検索で置き換えられた、または検索の取り消し・対象文書の切替などで失効した場合に返す。いずれも待機成功を発生させず、他の世代の`pending: false`や結果で代用しない。`query_generation_stale`の応答には、要求した値と現在の値を含める。呼び出し側は、新しい`ui.text`の結果から取得し直して再試行する。これらは要素の`ref`・`generation`による`target_stale`とは独立に判定する。

一括実行は最初の失敗で停止し、完了した手順と失敗した手順を返す。過去の編集は巻き戻さない。初期版の`stop_on_error`は`true`だけを受理する。

通信切断やタイムアウトでは、操作が実行済みの場合がある。編集・削除・保存を自動再送せず、同一セッションの要求IDから実行状況を照会できるようにする。セッションはIPC接続より長く存続し、接続の切断だけでは要求記録を失わない（§8.1）。要求記録はセッション内の有界な一時情報とし、永続的なworkflow stateを作らない。照会で確定できない結果は`unknown`として返す。

APIが入力の成否を確認できなかった場合も、成功と推測しない。

### 8.1 要求IDと実行状況の照会

#### セッションの開始・終了と再接続

- 開始：`gui-automation`を有効にした検証アプリの起動が、一つのセッションを開始する。セッションは、その起動で生成した接続トークン（§9.2）と一対一に対応する。
- 終了：検証アプリのプロセス終了、または隔離stateの片付けでセッションが終了する。終了時に要求記録とトークンを破棄する。
- IPC接続の切断はセッションを終了しない。切断だけで、進行中（`running`）や完了済みの要求記録を失わず、すでに配送を開始した操作も取り消さない。
- 再接続：呼び出し側は、同じ検証アプリのIPC接続先へ新しく接続し、起動時に受け取った同じ接続トークンを最初の要求で提示する（§9）。OSのアクセス制御と接続トークンの検証を通過した後に限り、`ui.request_status`で切断前の`request_id`を照会できる。
- 境界：アプリの再起動で作られる新しいセッションは、新しい接続トークンを持つ。新しいアプリ起動や新しい接続トークンから、過去のセッションの記録は照会できない。旧トークンは新しいセッションでは受理されず、過去の`request_id`の照会は`unknown`（`reason: "record_unavailable"`）となる。検証アプリが終了して応答できない場合、呼び出し側は成否を確定できず、再発行の前に現在の状態を確認する。

#### 要求IDの結び付け

副作用を持つ`ui.click`、`ui.key`、`ui.text`、`ui.batch`は、要求の最上位に呼び出し側が生成した`request_id`を必須とする。`request_id`が無い要求は何も実行せず`request_id_required`で失敗する。

- 呼び出し側は、最初の副作用要求を送る前に`request_id`を生成して保持する。応答を受け取れず切断しても、保持したIDで照会できるようにするためである。サーバーがIDを払い出す方式は、応答を失うと呼び出し側がIDを得られないため採用しない。
- `request_id`はセッション内で一意な不透明文字列（最大64文字）とする。同じ操作の再試行ではなく、新しい操作ごとに新しい値を使う。
- サーバーは、最初の手順を配送する前に`request_id`の記録を作る。記録を作れない場合は何も実行せず失敗する。
- 受付済みで完了していない要求（`not_started`のまま待機中の要求と`running`の要求）は、セッション内で同時に16件までとする（初期値。実装時に設定可能にしてよいが、上限なしにはしない）。上限に達している間に届いた新しい副作用要求は、記録を作らず、入力も配送せず、`request_limit_exceeded`で拒否する。応答には現在の未完了件数と上限を含める。拒否した要求の`request_id`は消費されず、呼び出し側は未完了の要求が完了してから、同じ操作を同じ`request_id`で送ってよい（記録が無いため`request_id_duplicate`にならない）。ただし、これは自動再送ではなく、拒否の応答を受け取って確認した上での明示的な再発行とする。拒否の応答を受け取れなかった場合は、`ui.request_status`が`record_unavailable`の`unknown`を返す。この場合も`unknown`は未実行の証明ではないため、現在の状態を確認してから再発行を判断する。
- 既に記録がある`request_id`を持つ要求は実行しない。`request_id_duplicate`で失敗し、現在の状態を応答に含める。これにより、再送が誤って届いても操作は二重に実行されない。
- `ui.batch`の`request_id`は一括実行全体を指す。各手順の`id`（例: `enter-query`）は同じ一括実行内の相関用であり、`request_id`の代わりにはならない。
- 読み取り専用の`ui.query`、`ui.inspect`、`ui.wait`、`ui.capture`、`session.info`と、下記の`ui.request_status`は`request_id`を要求しない。

要求の例（§5の一括実行に`request_id`を付けたもの）：

```json
{
  "method": "ui.batch",
  "request_id": "req-20",
  "params": {
    "window": "main",
    "stop_on_error": true,
    "steps": [
      {
        "id": "enter-query",
        "method": "ui.text",
        "params": { "expected_focus": "document-find-query", "text": "見出し" }
      }
    ]
  }
}
```

要求を受け付けて完了まで応答できた場合、応答は同じ`request_id`と各手順の状態を含む。

```json
{
  "request_id": "req-20",
  "result": {
    "status": "succeeded",
    "steps": [
      { "id": "enter-query", "status": "succeeded", "query_generation": "q-31" }
    ]
  }
}
```

#### 状態の定義

`request_id`全体と各手順の状態は次のいずれかとする。

| 状態 | 意味 |
|---|---|
| `not_started` | 記録は存在し、その要求または手順の配送を開始していないことを記録で確認できる |
| `running` | 配送を開始し、完了または失敗を記録していない |
| `succeeded` | 入力の配送が完了した（§7のとおり、期待した結果になったことの証明ではない） |
| `failed` | 失敗を記録した。エラーコードを含む。一括実行が停止した後の未配送の手順は`not_started` |
| `unknown` | 実行済みか判断できない |

配送の途中でAPIが成否を確認できなかった手順は`unknown`とする。`unknown`は`not_started`や未実行の証明として扱わず、成功とも推測しない。

要求全体の状態は、手順状態の組み合わせから次の優先順位の最初に該当する行で一意に決める。

| 優先 | 手順状態の組み合わせ | 要求の状態 |
|---|---|---|
| 1 | いずれかの手順が`running` | `running` |
| 2 | `running`が無く、いずれかの手順が`unknown` | `unknown` |
| 3 | `running`・`unknown`が無く、失敗した手順がある | `failed` |
| 4 | 全手順が`succeeded` | `succeeded` |
| 5 | 全手順が`not_started` | `not_started` |
| 6 | `succeeded`と`not_started`だけが混在（失敗・成否不明なしで配送が進行中） | `running` |

- `unknown`は`failed`より優先する。成否不明の手順は実行済みの可能性があり、要求全体を`failed`（実行されなかったことの確定）や`succeeded`と見せてはならない。`unknown`を含む要求は、`not_started`・`succeeded`・再送可の証明として扱わない。
- 要求全体が`unknown`でも、`steps`には各手順の状態を残す。既に`succeeded`として配送が完了した手順、`failed`の手順とそのエラーコード、`unknown`の手順をそれぞれ消さず、呼び出し側が配送済みの手順を把握できるようにする。
- 要求全体が`unknown`の場合、同じ`request_id`は再送せず、新しい`request_id`での再発行は`ui.inspect`などで現在の状態を確認した上での明示的な判断とする（§8.1「切断・タイムアウト時の契約」）。
- 例：手順が`[succeeded, unknown, not_started]`なら要求は`unknown`、`[succeeded, failed, not_started]`なら`failed`、`[succeeded, unknown, failed]`は`unknown`、`[running, unknown]`は`running`（完了後に再評価して`unknown`となる）。

```json
{
  "result": {
    "request_id": "req-21",
    "status": "unknown",
    "reason": "delivery_unconfirmed",
    "steps": [
      { "id": "focus-editor", "status": "succeeded" },
      { "id": "enter-query", "status": "unknown" },
      { "id": "confirm", "status": "not_started" }
    ]
  }
}
```

要求記録は現在の保持上限（未完了16件など）の範囲内の有界な一時情報であり、上記の集約のために記録や手順情報を追加で保持しない。

#### 状態照会`ui.request_status`

`ui.request_status`は、同一セッション内で`request_id`から状態を読む読み取り専用のメソッドである。照会は元の操作を再実行せず、入力の配送・文書・UI状態の変更も行わない。UIスレッドの操作列に並ばず、実行中の`ui.batch`があっても応答できる（`ui.wait`と同様にUIスレッドを占有しない）。

```json
{
  "method": "ui.request_status",
  "params": { "request_id": "req-20" }
}
```

```json
{
  "result": {
    "request_id": "req-20",
    "status": "running",
    "steps": [
      { "id": "enter-query", "status": "running" }
    ]
  }
}
```

次の場合は、同じ形式で`status`を`unknown`とし、理由を`reason`に含める。

- 記録が存在しない（そのセッションに送られていない、または受け付ける前に切断した）。
- 保持上限または期限により記録が破棄された。
- 配送の成否をAPIが確認できなかった。

```json
{
  "result": {
    "request_id": "req-20",
    "status": "unknown",
    "reason": "record_unavailable"
  }
}
```

記録が無い`unknown`は、操作が実行されていないことを意味しない。呼び出し側は、`ui.inspect`などの読み取り専用メソッドで現在の状態を確認してから、新しい`request_id`で操作を再発行するかを判断する。

#### 記録の保持

- 記録は同一セッション内のメモリ上だけに保持する。ファイル・設定・成果物には書かず、永続的なworkflow stateを作らない。セッション終了時に破棄し、別のセッションからは照会できない。IPC接続の切断では破棄しない。
- 記録するのは`request_id`、手順の`id`・メソッド名・状態・エラーコード・`query_generation`だけとする。入力文字列、本文、秘密入力欄の内容は含めない（§9）。
- 保持上限は直近256件、完了後の保持期限は10分とする（初期値。実装時に設定可能にしてよいが、上限なしにはしない）。上限を超えた場合は完了済みの古い記録から破棄する。`running`の記録は破棄しない。代わりに、未完了の要求を上記の同時16件に制限することで、`running`の記録を含む保持量を有限に保つ。未完了の要求は256件の上限より十分小さいため、上限を超えても破棄できる完了済みの記録が常に存在する。
- 期限切れ・破棄後の照会は`unknown`（`reason: "record_unavailable"`）とし、`not_started`や未実行の証明として扱わない。

#### 切断・タイムアウト時の契約

切断または`timeout`で応答を受け取れなかった場合、呼び出し側は同じ要求を自動再送しない。まず同じ接続トークンで再接続し、保持した`request_id`で`ui.request_status`を呼ぶ。`running`なら照会を繰り返して完了を待ち、`succeeded`・`failed`は結果として扱う。`unknown`は結果として提示し、再発行するかは現在の状態を確認した上での明示的な判断とする。同じ`request_id`での再送は§8.1のとおり実行されない。

#### 既存の相関値との区別

`request_id`は操作要求の実行状況を照会するための値であり、`ref`・`generation`（要素の同一性）や`query_generation`（検索クエリの世代）の代わりにならず、逆も同様である。`ui.request_status`の結果は、`target_stale`・`composition_active`・`query_generation_mismatch`・`query_generation_stale`の判定を変更しない。IME composition guard、`ui.wait`のUIスレッド外実行、§9のOS別アクセス制御と接続トークンは`ui.request_status`にも同様に適用し、トークンを提示しない接続は照会もできない。

## 9. 接続と有効化

検証用の`gui-automation`ビルド機能と、起動時の明示指定の両方で有効にする。通常の配布ビルドでは受け付けない。

通信はローカルIPCとする。

- macOS：Unix domain socket。
- Windows：名前付きパイプ。
- 共通：バージョン付きJSONの要求・応答。

接続先は検証アプリの起動時に取得し、プロセス・ビルド・隔離stateとの対応を確認する。同一利用者のアクセス制御と起動ごとの接続トークンを使用する。

### 9.1 OS別のアクセス制御

APIは画面入力と撮影を許すため、「同一利用者」をOSごとの具体的な仕組みで強制する。トークンはOSのアクセス制御に代わるものではなく、その上に重ねる二重の防御とする。

- macOS：Unix domain socketを、隔離state配下の現在の利用者専用ディレクトリに作成する。親ディレクトリは`0700`、socket本体は`0600`とし、所有者を現在の利用者にする。作成直後に権限と所有者を確認し、期待と異なる場合は待受を開始せず起動を失敗させる。共有の一時ディレクトリ直下には作成しない。
- Windows：名前付きパイプを、現在の利用者のSIDだけに接続を許可する明示的なACL（DACL）付きで作成する。他の利用者・Everyone・匿名・ネットワーク経由のアクセスは許可せず、リモートクライアントも拒否する。既定のACLには依存しない。ACLを設定できない場合は待受を開始せず起動を失敗させる。
- 共通：OSの制限を通過した接続でも、最初の要求で正しい接続トークンを提示しない限り、いかなるメソッドも実行しない。

### 9.2 起動ごとの接続トークン

- 生成：検証アプリの起動ごとに、OSの暗号論的乱数から十分な長さのトークンを新しく生成する。固定値や再利用は行わない。
- 配送：検証アプリを起動した操作側にだけ、同一利用者専用の経路で渡す。具体的には、上記の保護された接続先ディレクトリ（macOS）または現在の利用者だけが読める隔離state内のファイル（Windows）に、利用者限定の権限で書く。コマンドライン引数・環境変数・共有の一時領域には置かない。
- 保存場所と寿命：隔離state内の利用者限定ファイルに限り、リポジトリ・設定・成果物には保存しない。検証アプリの終了時、または隔離stateの片付け時に削除する。
- 秘匿：トークン値は通常ログ、エラー応答、画面撮影、証跡、本設計文書に出力・記録しない。失敗時は「認証失敗」の事実だけを残す。

### 9.3 未承認接続の受入確認

次を、実OS上のGUI検証のscenarioに含める。いずれも画面入力・撮影が実行されないことと、通常ログにトークン値が現れないことを合わせて確認する。

- macOS：
  - 別の利用者として、socketへの接続が権限エラーで拒否される。
  - socketと親ディレクトリの権限・所有者が上記の契約どおりである。
  - 同一利用者でもトークンなし・誤りのトークンの接続は、メソッドが実行されず拒否される。
- Windows：
  - 別の利用者として、名前付きパイプへの接続がアクセス拒否になる。
  - パイプのDACLが現在の利用者のSIDだけを許可し、リモート接続を拒否する設定である。
  - 同一利用者でもトークンなし・誤りのトークンの接続は、メソッドが実行されず拒否される。

任意コード・任意メソッド実行は提供しない。秘密入力欄の内容は取得・記録せず、通常ログにも入力文字列を残さない。画面撮影は隔離stateとダミー情報で行い、秘密表示中は取得しない。

## 10. 実装の配置と既存作業との関係

APIの受付と接続管理はapp層、対象登録と観測はUI層に置く。操作の配送はUIスレッドへ直列化し、既存の入力先判定や保存処理はそのまま利用する。

`ui.wait`はこの直列化の対象にしない。IPCの読み取りと期限付きの待機はUIスレッド外で実行し、UIスレッドを同期的にブロックしない。UIスレッドで行うのは、待機条件に必要な状態の取得、または完了通知の受け渡しだけとする。待機中も検索などの待機対象の状態更新がUIスレッドで進み、待機が自ら条件成立を妨げて期限切れにならないようにする。操作の配送（`ui.click`など）はUIスレッドで順に実行するが、待機はその実行を占有しない。

GPUIとの接続は、描画位置の取得、ヒットテスト、通常イベントと文字入力の配送に必要な最小範囲とする。

既存の`debug_bounds`はテスト専用であり、可視性・遮蔽・無効状態も不足する。そのまま正式な操作APIにはしない。汎用的なアクセシビリティ基盤の新設も今回の範囲に含めない。

AADWの新しいworkflow、状態管理、受入Gateは追加しない。既存のローカル／Hosted GUI検証から呼べるようにする。

関連作業は[#567](https://github.com/hide212131/hane/issues/567)（EditorView分割）、[#304](https://github.com/hide212131/hane/issues/304)（入力先判定）、[#310](https://github.com/hide212131/hane/issues/310)（GUI検証整理）。調査時点のopen PR [#638](https://github.com/hide212131/hane/pull/638)は初期化分離、[#655](https://github.com/hide212131/hane/pull/655)は外部変更対応であり、APIの初期化・文書状態観測の変更時に重なりを確認する。

設定画面は分離済みの`view/settings_screen.rs`を使い、入力先判定をAPI側へ複製しない。[並行開発手順](../refactor-execution-plan.md#feature-development-policy)に従い、関連する責務・状態・契約だけを調整する。全域リファクタリング全体の完了は待たない。

## 11. APIで確認する範囲と実OSで確認する範囲

| API中心で確認するもの | 実OSで引き続き確認するもの |
|---|---|
| ボタン・メニューの処理 | OSからアプリへの入力配送 |
| アプリ内のフォーカス移動 | 日本語IMEの変換・候補・確定 |
| キー割当と入力先の分離 | ネイティブファイルダイアログ |
| 確定文字入力、Undo/Redo | Finder／Explorerからのドロップ |
| 検索・設定・保存の状態変化 | 実際の描画・文字欠け・重なり |

API経由のテスト結果には入力経路を記録する。画面の正しさが受入条件なら、APIの状態取得とスクリーンショットを併用する。

検証方針は[Local GUI validation](../local-gui-validation.md)、[ADR-0024](../adr/0024-local-gui-validation.md)、[Commander Policy](../aadw-command-policy.md)に従う。Windowsの実OS GUI検証は接続先Windows環境から直接行う。

## 12. 導入順序と受入条件

最初は、一般設定と文書内検索を対象にする。既存IDを利用でき、ボタン操作・文字入力・フォーカス・非同期完了を小さい範囲で検証できる。

1. 接続、対象取得、クリック、キー、文字入力、状態取得、待機、一括実行、撮影を実装する。
2. テーマ切替と文書内検索の代表シナリオを移行する。
3. 本文編集・保存、名前変更、サイドバー操作へ広げる。
4. 必要性を確認してホバー・ドラッグ・スクロールを追加する。

最初の実装用Issueは手順1・2を完了範囲とする。本文の確定文字入力・Undo/Redo・保存・再読込は入力経路の保護確認として実施するが、本文の任意位置指定や全既存シナリオの移行は含めない。手順3・4は後続の明示的な範囲として扱い、最初のIssue完了をAPI全領域の完成としない。

初期受入では、正常動作に加えて次を確認する。

- [ ] クリック処理を無効にした場合、期待状態の確認が失敗する。
- [ ] フォーカスが別の欄にある場合、文字を入力しない。
- [ ] 入力先がIME composition中の場合、`ui.text`は文字入力イベントを配送せず、compositionをcommit・cancel・置換せず、本文・入力欄・Undo履歴・composition stateを変更しないで`composition_active`を返す。compositionがactiveでない場合の通常の確定文字列入力は維持する。
- [ ] ダイアログの背後や失効した要素を操作しない。
- [ ] 古い`ref`・`generation`を指定した`ui.inspect`・`ui.click`・`ui.wait`は、同IDの現行要素へ再解決せず、画面入力も待機成功も発生させずに`target_stale`を返す。
- [ ] 入力がUndo履歴や保存処理へ通常どおり反映される。
- [ ] 開始時に存在しない対象を待つ`ui.wait`は、`appear`で親対象の`ref`・`generation`と`match`条件を受け取り、待機anchor以降に登録された候補だけを束縛する。出現後の置換は`target_stale`、複数候補は`target_ambiguous`、不出現・状態不一致は`reason`付きの`timeout`を返し、同じ`id`の別要素では成功しない。
- [ ] 検索欄への`ui.text`が、開始した検索の`query_generation`を返し、検索完了を待つ`ui.wait`が同値を必須指定する。先行検索の`pending: false`や古い結果では成功せず、不一致は`query_generation_mismatch`、失効は`query_generation_stale`を返す。
- [ ] 保存の完了を対象文書・リビジョンに結び付け、過去の結果を利用しない。
- [ ] 一括実行が途中で止まり、通信切断・タイムアウトで編集を重複実行しない。切断後は自動再送せず`ui.request_status`で状態を照会でき、記録が無い・期限切れ・成否不明の場合は`unknown`を返して`not_started`と扱わない。`request_id`の無い副作用要求は`request_id_required`、記録済みIDの再送は`request_id_duplicate`、未完了要求が上限に達している場合は`request_limit_exceeded`で、いずれも実行しない。切断後も同じセッションなら同じ接続トークンでの再接続で照会でき、新しいアプリ起動や新しいトークンからは過去の記録を照会できない。
- [ ] macOS／Windowsの実アプリで代表シナリオが成立する。
- [ ] 通常配布ビルドではAPIが有効にならない。
- [ ] 秘密入力が状態取得・通常ログへ出ず、隔離stateで検証できる。
- [ ] 既存のworkspace tests、Clippy、required CIと、変更に必要なfocused GUIを確認する。

実装着手時に入力・設定・描画に関係する進行中PRとの重なりを再確認する。同じシナリオで移行前後の所要時間、画面取得回数、ツール呼び出し回数、取得可能なトークン使用量を比較し、測定条件と削減量を報告する。画像削減のために必須の実OS・画面検証を省略しない。
