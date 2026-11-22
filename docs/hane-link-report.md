# whisparrot 連携（Hane 側 H-1〜H-4）実装レポート

設計書（whisparrot リポジトリの `docs/design-hane-integration.md`）の §2 接続方式、§3 Hane API、§5 セキュリティ、§6 の H-1〜H-4 を実装した。ブランチは `feat/whisparrot-link`（`origin/main` の `28d36af0` から分岐、push・マージはしていない）。H-5（スナップショット）以降は範囲外。

**この実装は macOS 上で作業し、Windows の実機では何も動かしていない。** Windows 専用コード（名前付きパイプ、DACL、`GetTickCount64`、プロセス作成時刻、HWND 取得）はこの環境でコンパイルすらできていない（[未検証事項](#未検証事項)）。パイプの疎通を含む Windows 実機での結合検証は別途行う前提。

## コミット

| コミット | 内容 |
| --- | --- |
| H-1 | `hane-link` crate（フレーム・認証・版交渉・台帳・期限・接続処理・記述子・Windows パイプ）、連携許可の設定とトグル、HWND 登録と GPUI ディスパッチ |
| H-2 | `context.get`（近傍・タイトル・Front Matter・対象検証・未保存タブ・本文フォーカス） |
| H-3 | `edit.insert`（一単位の Undo、再送識別、範囲ハンドル発行）。`Editor` の履歴エントリ ID と連結用 API |
| H-4 | `edit.replace`（範囲再配置・検証、Undo の連結と独立差し替え）、`pendingRangeHandles` の追跡 |

## 構成

| 場所 | 役割 |
| --- | --- |
| `crates/link`（`hane-link`） | エディタにも UI にも依存しない部分。`frame`（長さ + JSON）、`auth`（資格情報・定数時間比較・SHA-256）、`message`（要求・応答・エラー・版交渉）、`server`（1 接続の認証と要求処理）、`service`（accept ループ）、`ledger`（操作台帳）、`host`（UI スレッドとの境界・適用権 `Ticket`）、`handles`（範囲ハンドル表）、`track`（範囲追跡）、`selection`（選択の座標変換）、`context` / `frontmatter`（文脈の切り出し）、`descriptor`（記述子の公開と掃除）、`runtime`（有効化・無効化）、`transport` / `testing`（接続の抽象とメモリ上の実装）、`windows`（Windows 専用: パイプ・DACL・プロセス情報）。 |
| `crates/editor` | `replace_range_recorded_with_entry`（履歴エントリ ID を返す記録つき置換）、`replace_range_amending`（直前エントリへの連結）、`can_amend_history_entry`。 |
| `crates/session` | `Settings.whisparrot_link`（`settings.conf` の `whisparrot_link=true`、未設定・不正値・読取不能は OFF）。 |
| `crates/ui` | `view/whisparrot_link.rs`: `EditorView::link_execute`（3 メソッドの実行）、`run_link_call`（HWND の照合と GPUI での実行）、設定画面のトグル（`settings_screen.rs`）。 |
| `crates/app` | `whisparrot_link.rs`（Windows のみ）: サービスの所有、HWND 取得、GPUI 上の実行ループ、終了時の記述子削除。`main.rs` に配線。 |

### 実行の流れ

1. 接続ごとのスレッドが長さ付きフレームを読み、`hello` を認証し、以降の要求を一件ずつ処理する（GPUI スレッドの外）。
2. 読み取り・編集の要求は `QueueHost`（容量 16）経由で GPUI スレッドへ渡る。満杯・受信側なしは `BUSY`（未適用）。
3. GPUI スレッドは HWND を登録済みの窓に照合し、**同じターンの中で** 適用権（`Ticket`）の取得、期限の再確認、対象・版・選択の検証、編集、`after_input` までを行ってから応答する。
4. 接続側は期限（編集は `deadlineTick`、読み取りは 3 秒）までに GPUI が着手しなければ適用権を放棄し、`DEADLINE_EXCEEDED`（`not_applied`）を返す。着手済みなら完了を待ち、待ちきれなければ `unknown`。放棄と着手は一回の比較交換で決まるため、「未適用」と答えた要求が後から適用されることはない。

## ワイヤ形式（whisparrot 側の実装用）

フレームは設計どおり `u32 LE の長さ + UTF-8 JSON`。認証前は 4 KiB、認証後は 256 KiB が上限で、長さを読んだ時点で検証し、確保前に拒否する。編集テキストは 32 KiB（UTF-8）まで。

### 記述子

`%LOCALAPPDATA%\Hane\integration\whisparrot\<pid>.json`（一時ファイルを書いて fsync し、名前を置換する）。

```json
{
  "schema": 1,
  "pid": 1234,
  "processCreationTime": "133500000000000000",
  "pipeName": "Hane.Whisparrot.1234.<instanceId>",
  "instanceId": "<32 桁の 16 進>",
  "token": "<64 桁の 16 進（256 ビット）>",
  "protocol": { "min": {"major": 1, "minor": 0}, "max": {"major": 1, "minor": 0} }
}
```

- `processCreationTime` は Hane プロセスの作成時刻（FILETIME、100 ns 単位の十進文字列）。クライアントは `OpenProcess` + `GetProcessTimes` で照合する。
- `pipeName` は `\\.\pipe\` を除いた名前。クライアントは固定形式 `Hane.Whisparrot.<pid>.<32 桁 16 進>` に合うものだけを受け入れ、`GetNamedPipeServerProcessId` で `pid` と照合してからトークンを送る。
- トークンと `instanceId` は有効化のたびに作り直す。無効化で記述子を削除し、再有効化前のトークンは使えない。
- 正常終了（アプリ終了イベント）で自分の記述子を削除する。クラッシュの残骸は、次回の有効化時に「PID が存在しない、または作成時刻が一致しない」ものを掃除する。

### `hello`

```json
{"id":1,"method":"hello","params":{"token":"…","client":"whisparrot","protocol":{"min":{"major":1,"minor":0},"max":{"major":1,"minor":0}}}}
```

成功（認証後に合意版・機能・上限を返す）:

```json
{"id":1,"result":{"protocol":{"major":1,"minor":0},"instanceId":"…","features":["context.get","edit.insert","edit.replace"],"limits":{"maxFrameBytes":262144,"maxEditTextBytes":32768,"maxBeforeChars":1200,"maxAfterChars":200,"handleLifetimeMs":120000}}}
```

- 合意版は major が同じで minor の低い方。クライアントが要求する最小 minor を満たせなければ `UNSUPPORTED_VERSION`（認証後なので理由を返してから切断）。
- 認証に失敗する要求（トークン不一致、`hello` 以外の最初のフレーム、構文不正、不正な `params`）は、固定の `UNAUTHORIZED` を返して切断する。認証前に文書情報は一切返さない。
- 全フレームで、未知の任意フィールドは読み飛ばす。

### エラー

`{"id":…,"error":{"code":"…","effect":"not_applied"|"unknown"}}` のみ。本文・パス・例外文字列は返さない。

`INVALID_REQUEST` / `DEADLINE_EXCEEDED` / `BUSY` / `UNSUPPORTED_VERSION` / `UNSUPPORTED_DOCUMENT` / `STALE_TARGET` / `NOT_EDITABLE` / `STALE_CURSOR` / `STALE_VERSION` / `IN_PROGRESS` / `OPERATION_CONFLICT` / `RANGE_CHANGED` / `RANGE_UNVERIFIABLE` / `RANGE_EXPIRED`（設計どおり）と、設計にない `UNAUTHORIZED`・`INTERNAL_ERROR`（[逸脱](#設計からの逸脱と補足)）。

### `context.get`

要求: `hwnd`（数値）、`beforeChars`（既定・上限 1,200）、`afterChars`（既定・上限 200）、任意の `expectedDocumentId`、`expectedRevision`（十進文字列）、`includeSnapshot`、`pendingRangeHandles`（最大 2 件）。

応答:

```json
{
  "documentId": "…", "revision": "42",
  "cursorByte": 120, "selection": {"anchorByte": 120, "activeByte": 120},
  "title": "メモ.md", "isUntitled": false,
  "frontMatter": {"status": "present", "text": "title: x\n", "range": {"startByte": 0, "endByte": 14}},
  "before": "…", "beforeStartByte": 0, "beforeTruncated": false,
  "after": "…", "afterStartByte": 120, "afterTruncated": true,
  "pendingRanges": [{"rangeHandle": "…", "tracked": true, "startByte": 3, "endByte": 9}, {"rangeHandle": "…", "tracked": false}]
}
```

座標は UTF-8 の 0 始まり・半開区間。文字量は Unicode スカラー値で数え、UTF-8 の途中では切らない。改行は正規化しない。

### `edit.insert`

要求: `operationId`、`deadlineTick`（`GetTickCount64` の十進文字列）、`hwnd`、`documentId`、`expectedRevision`、`expectedSelection{anchorByte, activeByte}`、`text`。

成功: `documentId`、`insertedRevision`、`range{startByte,endByte}`、`rangeHandle`、`expectedTextHash`（挿入文字列の UTF-8 SHA-256、小文字 16 進 64 桁）、`expiresInMs`（120000）。

### `edit.replace`

要求: `operationId`、`deadlineTick`、`hwnd`、`documentId`、`rangeHandle`、`insertedRevision`、`expectedTextHash`、`replacement`。

成功: `documentId`、`revision`、`range`、`undoMode`（`merged` / `separate` / `none`）。

## 設計との対応と実装上の判断

### セキュリティ（§5）

- **パイプ**: `PIPE_REJECT_REMOTE_CLIENTS`、`FILE_FLAG_FIRST_PIPE_INSTANCE`（先に同名を作られていたら起動に失敗）、バイトモード、重複 I/O。DACL は明示指定で、現在のユーザーに `GA`（追加インスタンスの作成に必要）、ログオンセッションの SID に **`0x00100083`（`FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE`）だけ**を与える。`FILE_CREATE_PIPE_INSTANCE`（= `FILE_APPEND_DATA`）を含む `GENERIC_WRITE` は付けていない。
- **記述子**: ディレクトリとファイルは作成時から現在のユーザーのみの DACL（Windows）/ `0700`・`0600`（Unix、テスト用）。ディレクトリの連鎖に再解析ポイント（リンク）があれば拒否し、ファイルは `FILE_FLAG_OPEN_REPARSE_POINT` / `O_NOFOLLOW` で作る。既存のアプリのフォルダー（`%LOCALAPPDATA%\Hane`）の DACL は変更せず、新規作成した階層と最内のディレクトリだけを限定する。
- **トークン**: 256 ビット、定数時間比較、起動・再有効化ごとに更新。ログ・設定・環境変数に出さず、`Debug` 出力は伏せ字。エラー応答は固定コードのみ。
- 接続は同時 8 件まで。超過は `BUSY` を返して閉じる。

### 台帳・期限・再送（§3.4）

- 台帳は認証済みの同一インスタンスで共有する。同じ `operationId` で内容が同じなら、処理中は `IN_PROGRESS`（`unknown`）、完了済みなら記録した結果（エラーも）を返す。内容が異なれば `OPERATION_CONFLICT`。
- 記録は初回受付から 5 分間は消さない。容量 512 件で満杯なら、適用前に `BUSY` で拒否する（保持期間内の完了記録も、処理中の記録も追い出さない）。
- UI スレッドへ渡せなかった要求（キュー満杯など）は記録を取り消し、再送で適用できるようにする。
- 着手後に応答が間に合わなかった編集は `DEADLINE_EXCEEDED` + `unknown` を返し、記録は処理中のまま残す。UI スレッドの応答が遅れて届けば（最大 60 秒）記録を確定し、再送でその結果を返す。

### 範囲追跡と Undo（§3.3）

- 追跡は `RopeBuffer::deltas_since` の各差分に対し、`edit_end <= start` なら右/左へ移動（範囲先頭への挿入を含む）、`edit_start >= end` なら範囲外（末尾への挿入・削除を含む）、それ以外は `RANGE_CHANGED`。空の編集範囲（純粋な挿入）が範囲内部にある場合も変更として扱う（`RevisionDelta::transform_range` は空範囲を交差なしとして扱うため使っていない）。
- 追跡後に UTF-8 境界と、保持した原文との完全一致を検証してから適用する。対象の Undo はその後に Redo しても `RANGE_CHANGED`。差分の保持（4,096 件）を超えて遡れなければ `RANGE_UNVERIFIABLE`。
- Undo の連結は、**文書の版が挿入直後のままで**、かつ履歴の先頭が挿入時に記録したエントリ（ID 一致・開始位置・挿入長が一致・Redo なし）のときだけ行う。連結すると一回の Undo で発話全体が戻り、Redo は整文後の文字列を復元する。それ以外は独立した Undo 一単位（`separate`）。
- 選択は座標変換して保つ。範囲の手前は不変、範囲の末尾以降（挿入末尾のキャレットを含む）は長さの差だけ移動、範囲内は先頭からの同じ距離（超過時は新しい末尾）で、書記素境界に切り下げる。

### UI（§4.6 の Hane 側）

- 設定は `settings.conf` の `whisparrot_link`（既定 OFF）。設定画面の「連携」に、保存先と通信範囲を説明する文言つきのチェックボックスを追加した。切り替えるとサービスの起動・停止を行い、**起動できなかったら設定は変えない**（ステータス行に理由を出す）。起動時に ON なら起動する。
- サービスは Windows でのみ組み立てる。他のプラットフォームでは設定画面に連携の項目を出さない（設定値は保存・読み込みできる）。

## 設計からの逸脱と補足

設計書の数値・項目・エラーの意味は変えていない。設計が未定義の箇所で次のように決めた。

1. **`hello` の項目名**: 設計は「トークンと対応する major・minor の範囲」とだけ書いている。`token`、`protocol{min{major,minor}, max{major,minor}}` とした（`client` は任意で無視する）。旧 `minVersion` / `maxVersion` 形式は廃止した（後述の整合修正）。
2. **追加したエラーコード**: `UNAUTHORIZED`（設計は認証拒否に「固定コード」と書くのみで名前がない）、`INTERNAL_ERROR`（編集前の想定外の失敗、または着手後に応答できなかった場合）。クライアントは未知のコードを `unknown` 相当として扱える。
3. **`beforeChars` / `afterChars` が上限を超えた場合**: エラーにせず上限に丸める。
4. **`includeSnapshot`**: H-5 は範囲外。`expectedDocumentId` と `expectedRevision` が無ければ `INVALID_REQUEST`（設計どおり）、あれば通常の文脈に **`snapshotStatus: "not_supported"`** を付けて返し、全文は返さない。`hello` の `features` に `snapshot` は含めない。
5. **Front Matter**: 開始行があり閉じ行が先頭 8,192 文字に見つからない場合は `unavailable`（短い文書で単に閉じ行がない場合も区別しない）。`range` は開始行の `---`（BOM を除く）から閉じ行の末尾（改行を除く）まで。`text` は開始行の改行の直後から閉じ行の直前までを、改行を含めて返す。
6. **空の編集**: `text` / `replacement` が空なら `INVALID_REQUEST`（whisparrot の設計も空・同一の置換を送らない前提）。
7. **ハンドル表**: 窓ごとに最大 64 件（超過時は最古を捨てる）。ハンドルは 128 ビットの乱数の 16 進。
8. **窓の登録**: 現状 Hane は単一ウィンドウのため `window_id = 1` の一件を登録する。登録は一覧で持つので、複数窓は `LinkWindow` を足せば対応できる。
9. **`context.get` / `edit.insert` の状態の写像**: 更新再起動の凍結・IME 変換中・設定画面などの本文以外の占有・本文フォーカスなし → `BUSY`、前景でない窓 → `STALE_TARGET`、閲覧・比較表示 → `NOT_EDITABLE`、保存済みの `.md` 以外 → `UNSUPPORTED_DOCUMENT`。`edit.replace` は前景・本文フォーカス・設定画面を要求しない（IME 変換中と閲覧・比較表示、凍結は拒否）。
10. **時間の既定**: ハンドシェイク 3 秒、フレーム本体・書き込み 5 秒、認証後の無通信 10 分、読み取りの UI 待ち 3 秒、着手済み編集の完了待ち 5 秒、編集の最大待ち 30 秒。いずれも実機計測前の初期値。
11. **クライアントのプロセス確認**: サーバー側で `GetNamedPipeClientProcessId` による接続元の確認は行っていない（設計は接続先の検証をクライアント側に置いている）。トークンと DACL で保護している。

## テスト

結果は末尾の[テスト結果](#テスト結果)を参照。実機の GUI なしで次を検証している。

- **フレーム処理・認証**（`crates/link/tests/protocol.rs` と各モジュール）: 長さの上限と確保前の拒否、ゼロ長・途中切断、トークン不一致・未認証の要求・構文不正・認証前の巨大フレーム、ハンドシェイクのタイムアウト、版交渉、未知メソッド・不正パラメーター、32 KiB の境界、同時接続の上限、サービス停止時の切断と受付停止、再有効化で旧トークンが使えないこと。
- **台帳・期限**: 再送の結果再生・競合・満杯・処理中、適用権の取得と放棄の競合、期限切れ、未着手のタイムアウトは `not_applied`、着手済みは `unknown` と `IN_PROGRESS`、受け取れない場合は記録しない。
- **範囲追跡**（`track.rs`）: 前・先頭・末尾・後・内部・重なり・Undo・連続差分・負方向のずれ。
- **Undo 連結**（`editor` と UI の結合テスト）: 連結後の一回の Undo / Redo、途中に別編集があれば独立、IME 中は何も変えずに拒否、Redo 後・範囲が異なるときは連結しない。
- **UI の結合テスト**: 認証済みのメモリ上のパイプ → ディスパッチ → 実際の `EditorView`。`context.get`（絵文字・結合文字・CRLF・Front Matter・未保存タブ・保存済み `.md`/非 `.md`・タイトル切り詰め・フォーカス・凍結・閲覧モード・IME）、`edit.insert`（Undo 一単位・選択を消さない・各 STALE・再送・期限・タイピングとの Undo 順序）、`edit.replace`（連結・独立・先頭/末尾/内部/前/Undo 後・ハッシュ/版不一致・消費済み/期限切れ・別タブ/文書入替・同一本文・選択の追従・履歴欠落・二発話の順序）。
- **設定**: 既定 OFF、不正値・読取不能も OFF、保存・読み込み、トグルでのサービス起動・停止、起動失敗時に設定を変えないこと、クリック操作、サービスがない環境では項目を出さないこと。連携許可を切り替えるテストは `StateStores::memory()` と `EditorView::from_sessions` で隔離し、実設定ストアは使っていない。
- **記述子**: 原子的公開、権限（Unix）、リンク拒否、残骸の掃除（死んだ PID・再利用された PID・自分・一時ファイル）、有効化・無効化・再有効化・二重有効化・リスナー失敗・Drop。

## 未検証事項

1. **Windows 実機でのパイプ疎通（別途結合検証が必要）**。`crates/link/src/windows.rs` と `crates/app/src/whisparrot_link.rs` は、この環境（macOS、Windows ターゲットなし）でコンパイルしておらず、`windows-sys 0.59` のソースのシグネチャを読んで書いた。構文は `rustfmt` で確認したのみ。最初に Windows で `cargo check` が通ることを確認する必要がある。
2. パイプの ACL（SDDL: 現在のユーザー `GA` + ログオン SID `0x00100083`）が、別プロセスのクライアント（whisparrot）から実際に開けること、他のユーザー・他のログオンセッションから開けないこと、`FILE_FLAG_FIRST_PIPE_INSTANCE` が二重起動で機能すること。ログオン SID が取れない環境（サービス等）では現在のユーザーのみになる。
3. 重複 I/O の読み書き・タイムアウト・キャンセル、クライアントが先に閉じたときの `ERROR_BROKEN_PIPE`、サーバーが閉じた後にクライアントが残りを読めること（`DisconnectNamedPipe` ではなく `CloseHandle` で閉じている）。
4. 記述子の DACL（`D:P(A;OICI;FA;;;<user>)`）の適用、`SetNamedSecurityInfoW` での既存ディレクトリの限定、再解析ポイントの拒否。
5. 実 GPUI ウィンドウでの前景判定（`Window::is_window_active`）と本文フォーカス（`FocusHandle::is_focused`）、HWND の取得（`HasWindowHandle`）が whisparrot の `GetForegroundWindow` の値と一致すること。テストでは窓を `activate_window` した状態を使っている。
6. 実機の IME（変換中の拒否と、確定後の挙動）、実機での Undo / Redo のキー操作。
7. `GetTickCount64` による期限が両プロセスで一致すること、プロセス作成時刻の照合。
8. 性能: 認識完了から原文表示まで p95 250 ms 以下（§6）は未計測。`context.get` の大きな文書での所要時間、UI スレッドの負荷も未計測。
9. 実際の whisparrot クライアント（W-1 / W-2）との結合。ワイヤ形式は上記の記述に従って実装したが、クライアント側は未実装のため、項目名の食い違いは結合時に見つかる可能性がある。
10. 複数ウィンドウ・複数インスタンス（現状は単一窓）。

## テスト結果

実行環境は macOS。コマンドは `cargo test --offline --workspace --no-fail-fast`（全ターゲット、ドキュメントテスト含む、70 ターゲット）。

| 実行 | 合格 | 失敗 |
| --- | ---: | ---: |
| 全体（最終のコード） | 3,445 | 113 |
| 全体（H-4 コミット時点） | 3,446 | 112 |
| ベースライン（`28d36af0`）の `hane-ui` lib のみ | 1,137 | 112 |

失敗の内訳:

- **112 件は `hane-ui` の lib テストで、ベースライン（`origin/main` の `28d36af0` を別 worktree で実行）でも名前まで同一の 112 件が失敗する。** この変更で増えた失敗はない。分野は `view::tests`（71）、`view::task_checkbox`（11）、`view::reading_mode`（9）、`view::mermaid_diagrams`（9）、`view::clipboard`（6）、`view::basic_editing_keys`（3）、`image_paste` / `editor_context_menu` / `document_find`（各 1）。原因（この環境の GUI・フォント・クリップボード等への依存と思われる）は調べていない。
- **1 件（最終の全体実行のみ）**: `app_update::tests::snapshot_alone_neither_shows_nor_suppresses_the_prompt`（`hane` bin）が `Idle` と `Ready` の不一致で失敗した。同じコードで単独実行（3 回）と `hane` bin 全体の再実行（4 回、各 226 件）は全て合格し、H-4 時点の全体実行でも合格していたため、負荷時の揺らぎと判断している（`app_update` は今回の変更の対象外で、`main.rs` の変更は Windows 限定）。ベースラインでの再現確認はしていない。

今回追加したテスト（合格）:

| 対象 | 件数 |
| --- | ---: |
| `hane-link` lib（フレーム・認証・版交渉・台帳・ハンドル・追跡・選択・Front Matter・文脈・記述子・実行時・適用権） | 73 |
| `hane-link` `tests/protocol.rs`（接続のシナリオ） | 21 |
| `hane-ui` lib（`view::whisparrot_link`: 設定 4、`context.get` 16、`edit.insert` 13、`edit.replace` 19） | 52 |
| `hane-editor` lib（履歴の連結 API） | 5 |
| `hane-session` lib（設定） | 3 |

追加の確認:

- `cargo clippy --all-targets` は `hane-link`・`hane-editor`・`hane-session` で警告なし。`hane-ui` と `hane` では今回追加したコードへの指摘なし（既存コードの指摘は見ていない）。
- Windows 専用コード（`crates/link/src/windows.rs`）は `rustfmt --check` による構文確認のみ（型検査・コンパイル・実行はしていない）。

## 次の作業の目安

1. Windows 実機で `cargo check` / `cargo test -p hane-link` を通し、パイプの疎通（`hello` → `context.get` → `edit.insert` → `edit.replace`）を whisparrot の W-1 / W-2 と結合して確認する（設計 §6 の結合検証の項目）。
2. 実機の計測値（フレーム・認証・UI 待ちの時間、p95 250 ms）で、本レポートの初期値（逸脱 10）を調整する。
3. H-5（版つきスナップショット）。`includeSnapshot` の応答と `features` に `snapshot` を足す。

## whisparrot クライアント（W-1）とのワイヤ形式の整合修正

whisparrot 側の解釈部が前提にする形式を正とし、Hane 側を合わせた。

1. **記述子**: `processCreated` を `processCreationTime`（値は従来どおり FILETIME 100 ns の十進文字列）に改名し、`protocol` を `{"min":{"major":1,"minor":0},"max":{"major":1,"minor":0}}` の範囲にした。`schema`・`pid`・`pipeName`・`instanceId`・`token` は変更なし。
2. **`hello` 要求**: `{"token", "client"（任意・無視）, "protocol":{"min":…,"max":…}}` のみ受け付ける。旧 `minVersion` / `maxVersion` は廃止した（読めないため `UNAUTHORIZED` として閉じる）。応答は従来どおり。
3. **`context.get` の `pendingRanges`**: 各要素を `{"rangeHandle", "tracked", "startByte", "endByte"}` にした。追跡不能のときは `tracked: false` とし、座標は付けない（キー `rangeHandle` と `tracked` は常にある）。旧 `handle` / `status` は廃止した。
4. **再送と期限の順序**: 既に正しい順序だった（コード変更なし）。`crates/link/src/server.rs` の `run_mutation` は、最初に台帳の `begin` を引き、`Replay`（完了済みは期限後でも記録した結果を返す）、`InProgress`（`IN_PROGRESS`）、`Conflict`（`OPERATION_CONFLICT`）を返した後で、初めて `deadlineTick` を現在時刻と比較する。`DEADLINE_EXCEEDED`（未適用）は台帳に無い新規の操作が着手前に失効した場合にだけ返る。確認のため回帰テスト `a_resend_after_the_deadline_still_gets_the_recorded_result_and_a_changed_one_conflicts` を `crates/link/tests/protocol.rs` に追加した（完了済み操作を期限後に再送して記録済みの結果が返り、内容を変えると `OPERATION_CONFLICT`、エディタへの投入は 1 回のまま）。

更新したテスト: 記述子・`hello`（`runtime.rs`、`tests/protocol.rs`、`hane-ui` の `whisparrot_link` の試験治具）・`pendingRanges` の検証。設定を切り替えるテストは従来どおり隔離した状態（`StateStores::memory()` 流儀）を使い、共有の実設定ストアには触れていない。

### 検証結果

- `cargo test --no-fail-fast`（全ターゲット、ワークスペース全体）: **合格 3,559 件・失敗 0 件**（`hane-link` lib 73、`hane-ui` lib 1,301 を含む）。
- 失敗が 0 件のため、`origin/main` との切り分けは対象なし（「ブランチで失敗し main で成功」したテストはない）。なお上の「失敗の内訳」にある `hane-ui` の 112 件の失敗は、今回の実行環境では再現せず全て合格した。当時の失敗は環境依存だったと考えられる（原因は未調査）。
- 注意: 整形は `rustfmt` をファイル単位で確認した。ワークスペース全体への `cargo fmt --all` は無関係な差分を大量に生むため行わず、連携まわりの変更だけをコミットしている。
