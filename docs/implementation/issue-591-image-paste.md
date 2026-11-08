# Issue #591 画像貼り付けの実装記録

- Issue: https://github.com/hide212131/hane/issues/591
- 基準main: `1e0d345e46d8c1805659968f8fad1148b0ae37a8`
- ブランチ: `codex/issue-591-image-paste`
- 詳細設計: [画像貼り付け](../design/issue-591-image-paste.md)
- 関連採用済み設計: [#596 clipboard](../design/issue-596-rich-clipboard.md)

macOS / Windows のクリップボード実画像1枚を、文書隣接の自動作成 `images/` へ透過保持PNGとして排他保存し、通常Markdownの単独行画像参照で表示する先行フェーズの実装記録。PNG/JPEGファイルのドロップは #591 全体に残る後続範囲で、設定・不要画像削除・加工・遠隔取得・#596 のHTML変換は含めない。`Refs #591` で追跡し、このPRだけでIssueをcloseしない。

## 実装の構成

| 責務 | 場所 |
| --- | --- |
| decode→PNG、上限、排他保存、挿入形状、生成参照の識別、Save As用コピー（GPUI非依存） | `crates/ui/src/clipboard_image.rs` |
| 単一snapshotとdispatcher、非同期job、鮮度照合、記録付き置換、Save As連携 | `crates/ui/src/view/image_paste.rs` |
| 本文Pasteの入口（snapshotを一度だけ読む） | `crates/ui/src/actions.rs` の `Paste` |
| 無名draftの有効base（`target_directory`）を一箇所で接続 | `EditorView::document_base_directory` / `document_resource_resolver`（描画も同じ resolver） |
| platformのクリップボード読取（公開`read_rich_clipboard`と既存GPUI読取） | `vendor/gpui-pre-macos/src/pasteboard.rs`、`vendor/gpui-pre-windows/src/clipboard.rs`（各 `HANE-PATCH.md`） |

## 有限の上限（`clipboard_image.rs`、境界テストあり）

| 定数 | 値 | 根拠 |
| --- | --- | --- |
| `MAX_INPUT_BYTES` | 64 MiB | 4K スクリーンショットのPNGは10 MiB未満。非圧縮8K bitmap（128 MiB）は拒否する。 |
| `MAX_IMAGE_SIDE` | 16,384 px | `image::Limits` の幅・高さ。ヘッダ段階でallocation前に拒否される。 |
| `MAX_IMAGE_PIXELS` | 40,000,000 | 8K×5K。16bit RGBAでも320 MiB。デコード前に `dimensions()` で確認する。 |
| `MAX_DECODE_ALLOC_BYTES` | 512 MiB | `image::Limits::max_alloc`。 |
| `MAX_OUTPUT_BYTES` | 64 MiB | 出力側をカウントする `Write` で打ち切る。 |

decode・PNG変換・ファイルI/Oはすべて background executor で行い、UIスレッドで実行しない。`image 0.25.10`（Cargo.lock に既存。`png`/`jpeg`/`tiff`/`bmp`/`gif`/`webp`）を hane-ui の直接依存にした。EXIF向きは画素へ反映し、PNGにはorientationを持たせない。SVG・ICO・PNMは実ラスタ画像として扱わず、本文があれば本文を貼る。

## 保存先・名前・命名規則

- 保存済み文書は `<文書のディレクトリ>/images/`、workspace無名draftは既存の `target_directory`。cwdや作業フォルダrootは使わない。`images` が無ければ `create_dir` で作成する（文書ディレクトリ自体は作らない）。`images` が通常ファイルなら保存開始前に拒否する。
- 生成名は `hane-paste-<unix millis>-<8桁hex>.png`（ASCII）。一意性は名前ではなく `create_new` の排他作成に依存し、衝突時は別名を最大32回試す。書き込みが途中で失敗したときは当該jobが作ったファイルだけを削除する。
- 再読込後も生成画像と分かる狭い規則: 本文中の `](images/hane-paste-<数字>-<8桁小文字hex>.png)` だけ。manifestやMarkdown独自属性は持たない。手書きの `images/photo.png` などは管理しない。

## 貼り付けと履歴

- 本文Pasteは同一snapshotの実画像を優先し、画像なしならtext経路。検索・設定・renameなど本文外の入力は従来どおりtext。画像のdecode/保存に失敗してもtextへ黙ってfallbackしない。
- 通常段落または空文書だけを許可する（現在のblock indexで選択範囲が全て `Paragraph`）。リスト・引用・表・コード・見出し等は保存開始前に拒否して案内する。indexが現revisionに追いついていないときは再貼り付けを案内する。
- 単独行にするための前後改行だけを足し、1回の `Editor::replace_range_recorded` で挿入する。1 Undoで選択元本文へ、Redoで同じ画像参照へ戻り、画像ファイルは削除・再生成しない。
- 要求時に SessionId・DocumentInstance（workspace世代込み）・revision・selection・base・draft id・job番号を捕捉し、完了時に全て照合する。不一致なら参照を挿入せず、当該jobが作った未公開ファイルだけを削除する。連続Pasteは先に完了したjobが適用され、後のjobはrevision不一致で安全に中止される。状態表示は最新のjob番号のものだけが更新する。

## 無名文書・Save As

- workspaceの無名draftは先行保存や命名を要求しない。同directoryのH1命名・renameでも `images/` 参照は同じ相対位置のまま有効。
- 保存先のない単独無題だけ既存の初回保存ダイアログを開く。キャンセル・失敗では画像を作らず本文・選択を保つ。保存が完了し、本文・選択が変わっていなければ貼り付けを再開する。
- 別directoryへのSave Asは、本文が参照する生成画像を新しい `images/` へ先にコピーする。同名で内容が同一なら再利用、内容が違えば別名にして参照を更新する。コピーに失敗したときは保存せず、作成済みのコピーを削除し、元の本文・画像・未保存状態を保つ。コピー中に本文が変わった場合も保存を中止する。元画像は削除しない。コピー元と同名保存先の比較読みはどちらも `MAX_OUTPUT_BYTES + 1` バイトまでに制限する（metadataは早期拒否のみ）。コピー元が実読取で上限を超えたら`TooLarge`とし、上限を超える既存の保存先ファイルは同一内容として再利用せず、変更・削除もせずに別名で保存する。

## platformの共有公開snapshot（`read_rich_clipboard`）

`crates/ui/Cargo.toml` にtarget別の直接依存（`gpui-pre-macos` / `gpui-pre-windows`）を足し、両platform crateのrootから同じ形の公開APIを使う。GPUI core全体はvendor化せず、`Platform` traitと`ClipboardItem`の形も変えない。GPUIの`read_from_clipboard`・find pasteboard・ファイルパスコピーは従来のまま。

- `read_rich_clipboard() -> Result<Option<RichClipboardSnapshot>, RichClipboardError>`。`Ok(None)`は本当の空だけ。
- `RichClipboardSnapshot { item, html, html_problem, images }`。`item`は本文とファイルパス、`images`は独立したOS項目ごとの`Result<Image, RichClipboardError>`。HTMLは#596の同世代snapshotから取得し、無効・上限超過は`html_problem`へ分けて本文/画像を保つ。画像・HTMLを得るために再度clipboardを読まない。
- `RichClipboardError`は`Busy`・`ChangedDuringRead`・`InvalidFormat`・`TooLarge`。bounded retryは識別を潰さず、最終失敗をそのまま返す。
- macOS: `changeCount`の前後が同じ世代だけ返し、3回変化し続けたら`ChangedDuringRead`。`pasteboardItems`の各項目から優先形式（PNG→TIFF→…→SVG）を1つ選び、同じ項目の別形式は1枚にまとめ、別項目は別の要素にする。パス・本文・画像が併存しても画像を取り落とさない。項目が報告されないpasteboardは1項目として扱う。
- Windows: 1つの`ClipboardGuard`内で全形式を読む（世代は変わり得ず`ChangedDuringRead`は出ない）。開けなければ`Busy`。**OS制約**: Windowsクリップボードは単一のデータオブジェクトで独立した複数画像を区別できないため、`images`は最大1要素（PNG→JFIF/GIF→CF_DIB→その他）。別形式は1枚にまとめる。
- HTMLは8 MiBを上限に読み、WindowsはCF_HTMLのUTF-8 byte offsetを検証する。画像形式が宣言されているのにデータが取れない・空・不正DIBは`InvalidFormat`、64 MiB超は`TooLarge`（コピー前に判定）。他の形式で読めるときはそちらを使い、読める形式が無いときだけ画像エラーにする。HTMLの問題は同世代のplainへのfallback判定に使い、画像エラーはUI側で本文を変更せず通知する。

UIはこのsnapshotを共通Paste dispatcherへ渡す。通常Pasteは実画像を優先し、画像がなければ#596のHTML→Markdown/plain fallbackを行う。「プレーンテキストとして貼り付け」は画像を使わず、plainまたはHTMLから抽出した可視テキストを使う。

| clipboardの状態 | 本文Pasteの結果 |
| --- | --- |
| 実画像1枚（別形式は1枚に集約） | 画像を保存して参照を挿入 |
| 独立した実画像が複数 | 保存前に件数と1枚ずつの貼り付けを案内、本文・選択・履歴・ファイル不変。先頭だけの採用や合成はしない |
| 画像が`Busy`以外の理由で取り込めない（`InvalidFormat`/`TooLarge`） | 理由を通知、本文不変。同じ世代のtextへfallbackしない |
| 読取自体が失敗（`Busy`/`ChangedDuringRead`） | 理由を通知、本文不変。空として扱わない |
| 本当に空 | 何もしない |
| 画像なし（SVGだけ含む） | 従来のtext貼り付け |
| パス・本文・画像が併存 | 本文だけが実画像を優先し、パス文字列は貼らない |

GPUIの`ClipboardItem`（Linuxと単体テスト）は項目の独立性を表せないため、画像entry 1つを1項目として数える。この経路は`Option`しか返さないので空と失敗は区別できない（Linuxにnative実装は無い）。UI単体テストはGPUIのテストclipboardを使い、native成功は実OSで確認する。

## 既知の制約（要確認）

- Windowsでは独立した複数画像をOSが区別できないため、複数画像の案内は主にmacOSの複数pasteboard itemで起きる。
- Save As の参照更新は文書ごとの保存transactionとして扱い、アクティブ/背景タブを区別しない。コピー完了後も生きているエディタ本文は変更せず、`DocumentSession::arm_save_rewrite` で次の1回の書き込みだけが「参照を更新した本文」を保存する。書き込みが成功して受理されたときだけ、同じ更新（`RewritePlan`）を生きている本文・選択・Undo/Redo履歴の全状態に適用する（履歴への追加編集は積まず、履歴の全消去もしない）。履歴の各stateは更新後のstateへ写し、元の編集単位を保てる場合は保つ。
- 履歴の各stateへ `RewritePlan` を評価する計算は文書サイズ×Undo/Redo件数の全文parse/String再構築になるため、UI同期経路では行わない。コピー完了後に `Editor::rewrite_snapshot`（rope複製と履歴の複製だけ）を取り、背景スレッドで `RewriteSnapshot::prepare` が本文・全履歴stateの更新結果（`PreparedRewrite`）を作る。`begin_save_as_write` はticket（DocumentInstance/SessionId/revision/base）を再確認してから `arm_prepared_save_rewrite` で渡す。書き込み開始と受理は `Editor::rewrite_is_current`（document revision・履歴の件数・IME未開始が準備時と一致）の間だけ準備結果を使い、`Editor::apply_prepared_rewrite` は置換範囲の適用と履歴の差し替えだけでplanを評価しない。書き込み中の追加編集・Undo/Redo・IME開始で一致しなくなった場合は、従来の `apply_rewrite`（その時点の本文と履歴を評価）へ戻るため追加入力は失われないが、そのときだけ同期の全評価コストがかかる。準備中に文書が変わった場合は既存の「保存前に文書が変更された」中止経路でコピーを片付ける。
- 書き込み開始後に準備結果が古くなった場合（追加入力・Undo/Redo・IME）も、受理時に履歴全体のplan評価へ戻さない。履歴の各stepは内容が変わるたびに変わる識別子（統合・改名rewriteでも更新）を持ち、`PreparedRewrite` は準備元の識別子列（Undo古い順→Redo実行順）を持つ。`Editor::follow_prepared_rewrite`（`DocumentSession::finish_save` の追従）は、一致している間は従来どおり差し替えだけ、一致しない場合は現在の履歴と準備元で識別子が共通な先頭部分の更新結果をそのまま使い（planを評価しない）、それ以降に増えた/入れ替わったstepの前後stateと現在の本文だけをplan評価する（`History::rebased`）。評価回数は「保存中に増えたstep数＋1」であり、履歴件数や既存のstepには比例しない。Undo/Redoだけなら本文の1回のみ。IMEは従来どおり確定してから追従する。planが現在の本文で無効なら何も変えない。保存bytes・`SavedStale`・dirty・最新本文は従来と同じ。ここで追い付けないほど編集された場合も、増えたstepぶんの評価で有限に進み、同期の履歴全体の再計算はしない。なお書き込み前（準備後）に変わった場合は従来どおりUI側のticket再確認で中止する。
- 回帰検出は `crates/session/tests/save_as_rewrite_prepared.rs`。書き込み中の追加入力・Undo/Redo・複数編集・IMEについて、履歴件数が6と60/90のときのplan評価回数が増えたstep数で有界であること（件数に比例しないこと）と、通常経路と本文・選択・Undo/Redoの全stateが一致することを確認する。plan呼び出し回数のカウンタで、準備済みの開始・受理がplanを一度も評価しないことと、通常経路と本文・選択・Undo/Redoの全stateが一致することを確認する（実時間には依存しない）。
- 書き込みが失敗・競合・取り消し・古い結果になったときは本文・選択・履歴・dirty状態を一切変えず、そのSave Asが作成したコピーのうち他の文書が参照していないものだけを削除する。書き込み中の追加入力は保存結果に含まれず（`SavedStale`）、本文へ同じ更新を適用したうえで未保存のまま残る。別の保存が実行中のときは、コピーの所有権と更新を運べないためSave Asを中止して再実行を案内する。

## 未実施の検証（passとして扱わない）

この作業環境ではビルド・テスト・clippyを実行していない。Commanderが別途、current headで確認する。

- `cargo test --workspace --all-features --locked` と `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`、通常feature/Releaseのbuild。
- vendor変更のtest: `cargo test --manifest-path vendor/gpui-pre-macos/Cargo.toml --features runtime_shaders,font-kit,test-support --lib pasteboard`（`hane_clipboard_*`を含む）、`cargo test --manifest-path vendor/gpui-pre-windows/Cargo.toml --features test-support --lib hane_clipboard`。世代変化（`ChangedDuringRead`）とWindowsの`Busy`は決定的に再現できずtestしていない。実OSで確認する。
- macOS / Windows実機のスクリーンショット・画像アプリからの貼り付け、日本語IME、検索/設定/renameの通常paste、連続貼付、閉じる/切替、書込不可先、文書+imagesを一緒に移動して開く。
- 変更前後の大文書応答性（同じRelease/fixtureの入力・scroll・RSS/処理時間）。
- current-head CI と CodeRabbit の manual full review。

通常の意味判断はJev、製品変更はClaude、Commanderは客観guardと一actionの実行を担当する。開始時・push直前のexact-head guardを維持する。
