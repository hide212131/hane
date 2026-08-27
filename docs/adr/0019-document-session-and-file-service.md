# ADR-0019: DocumentSession と File I/O 境界

## ステータス

承認済み

## 日付

2026-08-28

## 背景

Phase 4 までの `EditorView` は、描画・入力に加えてファイル状態そのものを所有していた。
`file_path: Option<PathBuf>`、`saved_revision`、autosave 世代、保存 job の実行中フラグ、
recent files の永続化、相対画像の解決用 `document_directory` が、すべて GPUI の view 構造体の
フィールドとして並んでいた。

この形のままファイラーを実装すると、次が同時に起きる。

- ファイラーが発行する open / rename / move / delete が、`EditorView` のフィールドを直接
  書き換える経路になる。
- 「未保存のまま別ファイルを開いたらどうなるか」「保存中に外部が書き換えたらどうなるか」
  といった規則が、GPUI の view を起動しないとテストできない場所に置かれる。
- 複数ファイルを開くには、これらのフィールドをすべて複数持つ改修が必要になる。

RFP はファイラーを機能追加の対象に含めており、`docs/refactor-plan.md` の R3.75 は
その前提としてファイル境界を確定することを求めている。

## 決定

新しい crate `hane-session` を追加し、「どのファイルを開いているか、変更されているか、
いつ書き込むか、外部で変わったらどうするか」をすべてそこへ置く。`EditorView` は
その利用者になる。

依存方向は ADR-0002 を次のように拡張する。

```text
ui
 ├── editor
 ├── session
 ├── metrics
 └── presentation

session
 ├── document
 └── editor
```

`session` は GPUI にも Markdown parser にも依存しない。

### FileIdentity — 同一ファイル判定と表示名の分離

`FileIdentity` は「読み書きに使う path」と「同一性を判定する canonical path」を別に持つ。

- 同一ファイル判定は canonical path の一致のみで行う。表示名は判定に一切関与しない。
- 存在するファイルの canonical path は I/O 境界（`FileService`）が解決する。まだ存在しない
  path（Save As の対象）は `FileIdentity::lexical` が `.` / `..` を fs に触れずに正規化する。
- rename / move は `moved_to` で path だけを差し替える。document は触らない。
- 削除・外部変更は `FileStamp`（長さ + 更新時刻）の比較で `ExternalChange` として表現する。
  内容の写しは保持しない。

### FileService — 唯一のファイル境界

`FileService` は `load` / `save` / `stamp` の3メソッドだけを持つ同期 trait とする。

- 同期にするのは、executor の選択を UI 側に委ねるためである。UI は GPUI の
  background executor へ載せ、テストは runtime 無しで in-memory 実装を差し替える。
- `save` は必ず atomic write（一時ファイル + rename）で行う。
- 上書きの可否は `OverwriteGuard` として job が運ぶ。`ExpectStamp` は書き込み直前に
  disk の stamp を確認し、一致しなければ書かずに `SaveFailure::ExternalChange` を返す。
  `Force` は Save As と、ユーザーが上書きを確認した後の再試行にのみ使う。

起動時の最初の1ファイルだけは window 生成前に読むため同期読み込みのままとする。それ以降の
open と save はすべて background executor へ出す。入力処理がファイル I/O を待つ経路は無い。

### DocumentSession — I/O を含まない規則の置き場

`DocumentSession` は editor、`FileState`、保存済み revision、autosave 世代、実行中/待機中の
保存を持つ。I/O は行わず、要求（`SaveDecision` / `OpenDecision` / `FileEventOutcome`）を返す。

- **generation**: document を差し替えるたびに増やす。parse 索引・layout cache・実行中の
  保存 job など、document から導出したものはすべて generation で有効性を判定する。
  revision だけでは別文書の結果を弾けない（ADR-0005）。
- **保存は同時に1つ**: 実行中に届いた要求は「最後の1つ」だけを queue する。autosave の
  連打は末尾の1回に畳まれる。
- **ticket**: 保存要求は `SaveTicket`（generation + 連番 + revision）を持ち、結果適用時に
  一致を確認する。一致しない結果は `Superseded` として捨てる。
- **autosave**: 打鍵ごとに autosave 世代を進め、armed した ticket が古くなれば発火しない。
  無効・untitled・非 dirty のときは ticket を発行しない。
- **外部変更と削除は自動解決しない**: clean な session への外部編集は `ExternalEdit`
  （再読込可）、dirty な session への外部編集は `Conflict` を返すだけで、内容は捨てない。
  削除は `Missing` を返し identity は保持する。保存すれば同じ path に作り直せる。

`SessionSet` が複数 session と active を持ち、open 要求を「既に開いている → 切替」
「clean な active → 差し替え」「dirty な active → 拒否」に振り分ける。同じファイルを
開き直しても、その session の未保存編集は決して破棄しない。

### 永続設定は repository として分離する

`SettingsRepository` と `RecentFilesRepository` を別 trait とし、`StateStores` が
`Arc` の handle として両者を渡す。store は view より長生きしてよく、view の生成・破棄に
設定の読み書きが従属しない。将来の filer tree state は3つ目の repository として足す。

### ResourceResolver

相対 resource（現状は画像）の解決は `ResourceResolver` が session の file identity から
行う。process の working directory は参照しない。untitled 文書は base を持たず、相対 path は
相対のまま返す。

## 結果

- `EditorView` は `PathBuf` フィールド、atomic save、recent-files 永続化を所有しない。
- open / rename / delete / 外部変更 / autosave / 未保存の競合規則が、GPUI を起動せずに
  テストできる（`crates/session/tests/conflict_rules.rs`）。
- 複数 `DocumentSession` を保持・切替できる API が成立し、現行の単一 session の挙動は
  そのまま保たれる。
- 一方で、UI が触れる状態が session と view に分かれ、`self.editor` の直接参照は
  `editor()` / `editor_mut()` 経由になった。R4A 以降で view 側に残る派生状態
  （line cache、block index、scroll）は、session 単位で持ち直すか検討する。

## 検討した代替案

### `EditorView` のフィールドのまま複数ファイルへ拡張する

採用しない。ファイル規則が GPUI の view でしかテストできない状態が固定される。

### `FileService` を async trait にする

採用しない。crate に async runtime の選択を持ち込むことになる。同期 trait +
呼び出し側の executor で、GPUI とテストの両方が同じ規則を使える。

### 外部変更を検出したら自動で再読込する

採用しない。dirty な session の内容を失う。検出は行い、判断はユーザーに返す。

### 内容のハッシュで外部変更を判定する

採用しない。保存のたびに文書全体を読み直すことになる。長さ + 更新時刻で十分であり、
判定できない場合は `Unknown` として上書きを許す方向へ倒す。
