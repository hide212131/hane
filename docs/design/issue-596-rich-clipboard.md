# Issue #596: 書式付きコピーとMarkdown変換貼り付けの詳細設計

## 状態と契約

対象: [Issue #596](https://github.com/hide212131/hane/issues/596)。2026-10-28の設計基準は main `497419203834f2a7aca413d1cb7758b37eb9dff6`。
この文書は実装直前の設計案であり、製品コード・テストは未変更、実OS検証も未実施。設計専用Draft PRで提示し、Claudeへの実装依頼・merge・Issue closeは行わない。要求と受入条件はIssue、今回の具体的な実装仕様は本書を正本とする。将来の実装用PRは別途準備し、本設計PRを無断で転用しない。

範囲は外部HTML→Markdownの貼り付けと、Hane→外部の書式付きコピーの両方。表の完全変換、画像取り込み、動画、数式、任意HTMLの保存、RTFだけの入力からの書式復元は初回対象外。RTFしかない場合も提供されたplain textは利用する。HTMLを持たないアプリにも書式保持を保証するものではない。

## 1. 変更前の事実と依存

| 責務 | mainで確認した入口 | 設計上の扱い |
| --- | --- | --- |
| 通常のCopy/Cut/Paste | `crates/ui/src/actions.rs` | `selected_text()`とGPUIの`ClipboardItem::new_string` / `item.text()`を使用。本文だけを新しい変換へ接続する。 |
| 入力先・IME | `crates/ui/src/view/input_routing.rs` | `input_target()`、各入力欄のfocus guard、`composition_marked_range()`を再利用。本文以外へ変換を漏らさない。 |
| 編集の適用 | `EditorView::dispatch`、`Editor::replace_range_recorded` | 通常Insertはtyping historyへ結合し得る。貼り付けには独立したReplaceを使う。 |
| 既存の安全な範囲置換 | `crates/ui/src/view/editor_context_menu.rs` | #629 / PR #630で採用された範囲検証・IME guard・`after_input`を参考にする。URLリンク挿入コマンドの仕様は変更しない。 |
| Markdown解析 | `crates/markdown/src/parser.rs`、`syntax.rs`、`block_index.rs` | `parser_options()`が設定の正本。UIでMarkdownを再parseしない。 |
| GPUI型 | registryの`gpui-pre 0.3.6/src/platform.rs` | `ClipboardEntry`はString/Image/ExternalPaths。HTML型なし。core全体を追加vendor化しない。 |
| macOS | `vendor/gpui-pre-macos/src/pasteboard.rs` | `Pasteboard::read/write`、既存data helperとmetadata hashを再利用。現在は文字列を見つけると画像より先に返す。 |
| Windows | `vendor/gpui-pre-windows/src/clipboard.rs` | `ClipboardGuard`、`LockedGlobal`、format列挙、`set_clipboard_bytes`を再利用。現行readは複数形式を列挙する。 |

`Cargo.toml`はcoreに`gpui-pre =0.3.6`、platformに既存のmacOS/Windows patchを採用している。`vendor/gpui`は旧版の履歴であり変更先・検証先にしない。

関連する#591はOpen、画像貼り付けの実装PRは調査時点でなし。#547の閲覧モードと#629のリンク挿入はmainに存在する。Open PR #638はEditorView初期化の移動で、本機能の変換・clipboard責務に依存しない。#643はsidebar、#640はWindows GUIロックであり設計を待たせる理由はない。#297全体の完了は待たない。実装開始時にcurrent mainと関連PRを再取得し、clipboard/input責務の変更が先に受け入れられた場合は受入済み経路へ合わせる。

## 2. OS境界と内部データ

既存の二つのplatform crateへ、汎用のHTML付きclipboard拡張を追加する。各crateのrootから狭い公開APIをre-exportし、UIはtarget別の直接依存で呼ぶ。既存`Platform` traitや`ClipboardItem`のshapeは変えない。UIへnative APIを重複実装しない。既存GPUI経路のplain text・find pasteboard・ファイルパスコピーは維持する。

実装時に導入するAPIの契約（以下の名前は新設予定）:

- `read_rich_clipboard(source_format: &str) -> Result<Option<RichClipboardSnapshot>, RichClipboardError>`。
- `RichClipboardSnapshot { item: ClipboardItem, html: Option<String>, source_metadata: Option<String> }`。`item`内のtext/image/filesとHTMLは同じOS clipboard世代から取得する。HTMLはOS headerを除いたUTF-8 fragment。
- `write_rich_clipboard(plain: &str, html: Option<&str>, source_format: &str, source_metadata: Option<&str>) -> Result<(), RichClipboardError>`。同じclipboard項目の別表現としてまとめて書く。
- エラーは`Busy`、`ChangedDuringRead`、`InvalidFormat`、`TooLarge`、`WriteFailed`を区別。HTMLだけが不正ならplainを保持し、snapshotのHTMLを無効化して理由をUIへ返せる診断欄を持たせる。clipboardを開けない場合と空内容は区別する。

platform側はHTML解析・Markdown判定を行わず、native形式の検証とtransportだけを担当する。Hane固有metadataのJSON解釈・優先順位は`crates/ui/src/clipboard.rs`（新設）のadapterに置く。metadataのnative format名は呼び出し側の定数を渡せる契約にし、platformへHaneの意味判断を埋め込まない。

Haneは専用format `org.hane.clipboard.source.v1`へ`{"version":1,"markdown":"…"}`を保存する。macOSでは同名pasteboard type、Windowsでは同名の登録formatを用い、UTF-8 JSONを入れる。読取後、version・サイズ・JSON型と`markdown == item.text()`を検証する。失敗したmarkerは無視し外部入力として扱う。これは原文選択の印であり、送信元認証・信頼境界の昇格には使わない。既存GPUI metadataは上書き・流用しない。

macOSでは`Pasteboard::general()`を使い、plain/HTML/専用formatを一つのpublishで書く。readは`changeCount`を前後比較し、異なれば`ChangedDuringRead`、異世代のplainとHTMLを結合しない。HTML-onlyにも対応する。複数pasteboard itemを一つの書式付き文章へ推測結合しない。複数項目は既存plain経路へfallbackする。

Windowsでは一回の`ClipboardGuard`中で全形式を読む／書く。HTMLは登録名`HTML Format`、plainは`CF_UNICODETEXT`。CF_HTMLのheader・UTF-8本文・fragment offsetを検証し、`0 <= StartFragment <= EndFragment <= allocation length`、有効なUTF-8境界を必須とする。StartHTML/EndHTMLが-1の場合もfragment範囲は検証する。contextが存在すれば包含関係も検証する。headerはASCII、offsetは**文字数でなくUTF-8バイト数**。CR/LF/CRLFのheaderと先頭ゼロに対応し、不正offsetをmarker探索で救済して範囲外を読むことはしない。出力は固定桁offset、UTF-8 HTML、StartFragment/EndFragmentコメントを生成し、終端NULをoffsetに含めない。

公開APIはgeneric clipboard transportとして両platformで同じshapeにする。macOSは既存`unique()` pasteboardを受け取る内部helperを用意してunit testでgeneral clipboardを汚さない。macOSの所有権とautorelease lifetime、WindowsのGlobalAlloc/Lock/UnlockとSetClipboardData成功時だけの所有権移譲は既存helperを使う。publish前に全データのencode・size検証を終える。OS publish途中の失敗は成功扱いしない。clipboardの完全rollbackは保証できないためCutでは本文を削除せず短いエラーを表示する。

Linux等は今回native HTML保証の対象にせず、従来のGPUI plain経路を維持しcompile可能にする。macOS/Windowsの受入をそのまま他OSの証拠にしない。

## 3. 貼り付けの決定表

最初に既存focus guardと`input_target()`を適用し、本文なら`body_editable()`を確認する。短い入力欄は既存のplain経路を使い、閲覧モード本文はclipboardを読まず終了する。

| 本文での操作／snapshot | 結果 |
| --- | --- |
| プレーンテキストとして貼り付け | 提供されたplainをそのまま挿入。HTML-onlyならHTMLから安全に文字を抽出する。Markdown記号を生成しない。 |
| 通常貼り付け・有効なHane marker | plainの原文bytesを優先。HTMLから再生成しない。 |
| 通常貼り付け・外部HTML | §4でMarkdownへ変換する。 |
| 通常貼り付け・plainだけ | 従来どおり、その文字列を挿入。文字列中のHTMLらしい文字をHTMLと推測しない。 |
| HTMLが不正／変換不能・plainあり | 同じsnapshotのplainへfallbackし、書式を変換できなかった旨を短く表示。 |
| HTMLだけ・変換不能／サイズ超過 | 本文を変更せず理由を表示。空文字で選択を削除しない。 |
| 空snapshot／ファイル・画像だけ | #591未導入時は本文を変更しない。 |

#591導入後は同じsnapshotからnative画像の有無を判定し、通常貼り付けでは同Issueの画像優先契約を先に適用する。プレーンテキスト貼り付けは画像を保存しない。HTML内の`img`を画像データありと扱わず、URLから取得しない。Hane自身の文章コピーは画像データを併記しないため原文往復と競合しない。#591側で先にdispatcherができた場合はそこへ合流させ、二つのclipboard読取・優先順位判定を残さない。

`PastePlainText`を新設し、macOSはCmd+Shift+V、Windows/LinuxはCtrl+Shift+Vへ本文contextで登録する（既存`secondary`のOS対応を使う）。通常Pasteのキーは維持。本文の右クリックメニューにも「プレーンテキストとして貼り付け」を追加し、同じactionをdispatchする。短い入力欄の独自handlerを乗っ取らず、実装直前にcurrent binding衝突を再確認する。

## 4. HTML → Markdownの純粋変換

`crates/markdown/src/clipboard.rs`（新設）にOS非依存の変換器を置く。HTML parserにはlockfileに既存の`html5ever 0.27.0` / `markup5ever_rcdom 0.3.0`を直接依存として宣言し、断片をbody contextで解析する。正規表現によるHTML除去、WebView、JavaScript実行、ネットワーク、CSS cascade計算は導入しない。parser API・lockfile整合は実装時にbuildで確認する。

中間表現はParagraph/Heading/Strong/Emphasis/Strike/Code/CodeBlock/List/Quote/Link/Text/Breakだけとし、HTML属性は必要な意味へ変換した時点で捨てる。シリアライズ後のMarkdownを同crateの`parser_options()`で検証する。raw HTMLをそのまま出力しない。

| 入力 | 出力規則 |
| --- | --- |
| `p`、`div`等のblock container | 子を段落化し、block間は空行一つ。隣接文字を結合して失わない。 |
| `h1`〜`h6` | `#`〜`######` + 空白 + 内容。 |
| `strong/b`、`em/i`、`s/del/strike` | `**`、`*`、`~~`。前後の空白はdelimiter外へ出し、空要素は記号にしない。入れ子は構造を保持。 |
| `code` | 本文に含まれるbacktickの最長連続数+1のdelimiter、必要なpaddingを使用。 |
| `pre`（内部codeを含む） | 改行・空白を保持するfenced block。内容より長いbacktick fenceを使用。安全な`language-*` tokenだけinfoとして扱い、任意classは捨てる。 |
| `ul/ol/li` | `- `と`N. `。`ol start`の非負整数を保持。継続行・子blockをmarker幅に合わせindentし、入れ子と複数段落を保持。特殊な番号style・`li value`は対応不能としてplain fallback。 |
| `blockquote` | 子blockの各行を`> `でprefix。 |
| `a href` | 制御文字を拒否し`url`で検証した絶対HTTP(S)だけ`[label](destination)`。`<base>`やSourceURLから相対URLを解決しない。他scheme・相対URLはlabelだけ残す。 |
| `br` | Markdownのhard break（backslash+改行）。通常改行はHTMLの空白規則に従う。 |
| text/entity | entityをdecode。pre/code以外はHTMLのASCII空白を畳み、NBSP・日本語・結合文字・emojiを保つ。Markdownで意味を生む記号をescapeする。 |
| span等の装飾container | 子の文字と対応する構造だけ残し、style/class/font/colorは捨てる。 |
| table | 完全な表を生成せず、snapshotのplainへ全体fallback。plainがなければセルをtab・行を改行で安全に文字化する。 |
| img | altを文字として残す。srcの取得・画像保存はしない。 |
| script/style/iframe/object/embed/template/head、SVG/MathML | subtreeを捨てる。コメント、event handler、外部resource属性も出力しない。 |
| その他の未対応要素 | 安全な子textを保持。文字化が損失を招く場合はplainへ全体fallbackし、黙って一部だけ成功にしない。 |

Markdownのescapeはtext/label/destination/codeで分ける。`* _ ~ [ ] ( ) < > &`、行頭`# > - + 1.`、backslash、連続backtickをfixture化する。リンクdestinationは既存link insertionのescape規則を同crate内で共用できる範囲だけ抽出する。変換結果は選択位置へ文字列として挿入し、周囲の既存sourceを再整形しない。コード内へ貼る際も貼り付け文字列をそのまま置く。Markdownを構文として解釈させたくない場合はplain操作を利用する。

## 5. Markdown → HTMLと原文往復

Copyのplainは`editor().selected_text()`で得た原文をそのまま保持する。選択はbyteのSourceRangeを正とし、disclosureで隠れたmarkerや改行も失わない。HTMLと専用metadataは同じsource revisionとselectionから生成する。Cutにも同じコピー生成器を使うが、書込成功と対象鮮度の確認後にだけ範囲を一回削除する。閲覧モードCutは従来どおりclipboardにも触れない。

HTMLは対応する意味要素だけを生成する。`h1..h6/p/strong/em/del/code/pre/ul/ol/li/blockquote/a/br`、必要に応じた`ol start`と安全な`href`に限定する。theme・style・font・classを付けない。本文のraw HTMLはescapeした文字として扱い、scriptを外部アプリへ渡さない。未対応の表・数式・画像記法等は原文をescapeした文字として残す。

変換器は`hane-markdown`内で既存のoffset event streamと`parser_options()`を共用する。UIへ`NodeKind`の解釈や別parserを増やさない。純粋APIは`export_clipboard_html(source_snapshot, selection)`とし、同revisionのdocument snapshotを受け取る。選択と交差する完全なblockを解析対象にし、外側のreference定義は既存formal parseから得る。現行`LinkParse`だけでは全reference/code内容を復元できないため、必要なreference解決情報をmarkdown層の解析結果へ追加する。全本文をUI threadでString化しない。formal parseがまだfreshでなければそのsnapshotからbackgroundで必要な解析を完了する。

部分選択では、delimiterも含めて完全に選択された構文だけを意味要素にする。例えば`[羽](https://example.com)`全体はリンク、`羽`だけを選択したら単なる文字。選択が記号の途中ならその範囲の原文をescapeする。選択外のheadingやlink markerを補完しない。blockの途中の選択にはheading/list wrapperを推測付与しない。reference link全体の選択では定義が選択外でも解析済みのdestinationを使える。純粋変換のテストはfragment内の選択外文字が一文字も漏れないことも確認する。

原文保存の例: Haneで`__羽__`をコピーするとplainと専用metadataは`__羽__`、HTMLは`<strong>羽</strong>`。Haneへ通常貼り付けすると`__羽__`を保持し、外部由来の同じHTMLは`**羽**`へ変換する。外部が専用formatを落とした場合は外部HTML扱いになる。

## 6. 非同期処理・IME・編集transaction

clipboardのOS snapshot取得とfocus判定はUI threadで短く行い、HTML parse・Markdown exportをbackgroundへ渡す。DOM/HTML変換で文書全体のlayoutやshapingを実行しない。既存snapshotとjob executorを使い、永続queueや独自状態機械を追加しない。

変換jobは`SessionId + DocumentInstance + Revision + Selection(anchor/active) + operation generation`を捕捉する。選択の方向も比較し、範囲だけの一致で受理しない。完了時にはactive sessionが同じ、instance/revision/selectionが一致、本文にfocusがあり編集可能、IME未確定なし、同一最新generationであることを確認する。再読込・タブ切替・caret移動・新しい操作・文書closeで無効化し、古い結果をrebaseして挿入しない。閉じたsessionの再利用もDocumentInstanceで拒否する。新しいCopy/Cut/Pasteで旧jobを無効化する。

IME未確定中のPaste/Cutは未確定文字や選択を変更せず「変換を確定してから貼り付けてください」と案内する。Copyはsnapshotが安全に取得できる場合の原文だけを利用し、compositionを確定・cancelしない。OSの確定callbackで再試行・二重適用する仕組みは作らない。短い入力欄のIME処理は従来経路を保つ。

成功時は`Editor::replace_range_recorded(range, result, collapsed_selection_at_end)`を一回呼び、成功後に`EditorView::after_input`を一回呼ぶ。IMEがないことを事前条件とし、既存API内のcomposition処理に依存しない。既存`EditKind::Replace`でtyping groupと独立させ、直前・直後のtypingを一回Undoへ巻き込まないことを検証する。選択外のsource、保存状態、revision、cache通知は既存の更新経路に従う。失敗時に空置換を適用しない。

OSデータをRustへcopyする前にnative lengthを確認する。HTML入力・metadataは各8 MiB、DOMは100,000 node、深さ128、生成結果16 MiBを初期の処理上限とする。上限超過では同snapshotのplainへfallbackし、plainがなければ中止。plain-onlyの既存サイズ制限は増やさない。制限はUTF-8 bytesで数え、DOM構築中のnode/depthを制限できるTreeSinkを用いる（構築後だけのチェックでは不十分）。中止後のDOM破棄で深い再帰を起こさない。copy側もHTML上限超過時は原文だけを書き、長大選択を空にしない。metadataの上限も超えるときは専用formatを省略し、HTMLも併記しないことでHane内でもplain原文を優先できるようにする。処理中はstatusに変換中を表示し、無効化・成功・失敗時に同じgenerationの表示だけを解除する。clipboardの内容をログに出さない。

## 7. 実装箇所と順序

これは製品実装時の順序であり、この設計PRで実行する指示ではない。全段階が#596の完了に必要。

1. `crates/markdown/src/clipboard.rs`、同`lib.rs`/`parser.rs`/`syntax.rs`、同`Cargo.toml`とlockfile: import/exportの純粋API、reference情報、escape、制限を実装。Markdown parser設定は一箇所のまま保つ。
2. 現行platformの`pasteboard.rs` / `clipboard.rs`、各crate root、`HANE-PATCH.md`: rich snapshotとpublishの公開拡張、CF_HTML codec、所有権・失敗処理。既存helperを共用し旧GPUIを触らない。
3. `crates/ui/src/clipboard.rs`、`view/clipboard.rs`、`lib.rs`、`view.rs`のmodule宣言、target別`Cargo.toml`: adapter、job鮮度、metadata検証、source snapshot、status、置換を接続。#638の初期化移動を複製しない。
4. `crates/ui/src/actions.rs`、`view/editor_context_menu.rs`: Copy/Cut/Paste/PastePlainTextを共通入口へ接続。短い入力欄は既存handlerを使う。
5. 以下の自動・実OS検証、current-head CIとCommander Policyに従うreviewを行う。GUI不足のまま製品受入へ進めない。

API shapeの共有だけを理由にGPUI coreをforkしない。platformの公開APIを将来upstreamが提供したらそのAPIへ置き換え、patch解除条件をHANE-PATCH.mdに記録する。新たなADRが必要な全製品共通の保存形式は導入しない。

## 8. 受入条件と検証の対応

| Issueの完了条件 | 自動検証 | 実OSのfocused scenario |
| --- | --- | --- |
| 外部へ書式付きコピーとplain原文 | export全構文・Unicode・部分選択・HTML escape、native同時publish | Hane→ブラウザcontenteditable／文書アプリ、Hane→コードエディタで原文一致 |
| Hane内往復の原文不変 | marker検証、unknown version、不一致plain、CRLF、reference・`__`等の非正規記法 | Hane内・別Haneプロセスへコピー→貼り付け、保存→再読込で原文一致 |
| 外部HTMLをMarkdownへ変換 | §4全行のgolden fixture、入れ子・コードfence・日本語・リンク・entity・malformed HTML | ブラウザfixtureとWord/Google Docs等の実アプリから見出し／リスト／引用／コード／リンクを貼る |
| 入力欄・閲覧・IME・Undo/Redo・複数tab | 既存input_routing/reading_mode/IME回帰に新action追加。新旧job逆順、selectionのみ変更、reload、close、focus移動、失敗で不変 | 両OSの未確定日本語で安全な拒否→確定→貼付、選択置換→Undo→Redo、typing前後のUndo独立、tab切替中の変換 |
| 危険HTMLと外部取得の禁止 | script/iframe/SVG、危険href・制御文字・style、サイズ/深さ/node上限、ネットワーク入口なしを確認 | 外部resource付きfixtureをofflineで貼り、文字構造だけが入ることを確認 |
| 実clipboardとcurrent-headの品質 | native形式encode/decode・空/欠損/HTML-only・競合・書込失敗・不正CF_HTML offset、既存plain/image/files回帰 | macOS/Windowsの実clipboardで混在形式、HTML-only、plain操作、読み取り競合、Cut書込失敗を確認 |

Golden例（LF表記）:

- `<h2>羽</h2><p><strong>太字</strong>と<a href="https://example.com/">リンク</a></p>` → `## 羽\n\n**太字**と[リンク](https://example.com/)`。
- `<p>a<br>b</p>` → `a\\\nb`（a、backslash、LF、b）。
- `<p>&lt;script&gt; *x*</p>` → literal textとして表示されるMarkdown。実script要素とは区別する。
- `<a href="javascript:alert(1)">文字</a>` → `文字`。
- 選択`A__羽__B`内の`__羽__` → plain `__羽__` / HTML `<strong>羽</strong>`。選択`羽`だけ → HTMLの文字`羽`だけ。

実装前測定は同じOS・Release・fixture・測定条件で、現行plain Copy/Paste（1 KiB/100 KiB/1 MiB）と10 MiB文書内の短い選択のcopyについてUI占有時間・完了時間・RSSを記録する。実装後は同じplain測定に加え同程度のHTMLを測り、document全体長に比例したUI thread処理・保持量が増えていないことを確認する。#23原本の性能基準は維持し、この設計で未測定値の新gateを成功扱いしない。

実装時の主なコマンド:

- `cargo test -p hane-markdown --locked`、`cargo test -p hane-editor --locked`。
- `cargo test -p hane-ui --features gpui/test-support --locked`（追加した対象testをまず指定して実行）。
- `cargo test --workspace --all-features --locked`、`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`。
- macOS: `cargo test --manifest-path vendor/gpui-pre-macos/Cargo.toml --features runtime_shaders,font-kit,test-support --lib hane_clipboard`。
- Windows: `cargo test --manifest-path vendor/gpui-pre-windows/Cargo.toml --features test-support --lib hane_clipboard`。

`hane_clipboard`は追加予定のtest名prefixであり既存成功結果ではない。workspace外vendor testsをworkspace CIだけで実行済みとしない。GUIは`hane-gui-test`とADR-0024、失敗時は`docs/gui-test-recovery.md`を適用する。Windowsは許可済み既存WindowsチャットからWindows上で直接操作し、Mac経由VMは使わない。今回は設計までなのでWindows担当への依頼は送らない。製品受入前には対象head・必要なbase context・OS/app version・入力形式・期待/実際・artifactを保存する。

## 9. 実装開始時に確認する条件

未決の製品仕様は本設計に残さない。以下は実行環境・current factsの再確認であり、未確認のまま製品受入へ進めない。

- current main、#596本文、#591と同じclipboard責務のPR、#638の移動先を再取得する。
- 実装用PRを別途用意し、対象branchから本書を読めるようにする。未採用案を採用済みと表現しない。
- baseline測定とnative fixtureを準備し、macOSとWindowsの検証枠・操作経路を確保する。
- CommanderがJevの次action判断と客観guardを確認した後、利用者から実装開始の依頼を得た範囲でClaudeへ依頼する。この設計PRから自動起動しない。

## 参考資料

- [Commander Policy](../aadw-command-policy.md)、[並行開発手順](../refactor-execution-plan.md#feature-development-policy)、[Claude実行手順](../aadw-claude-execution.md)。
- [Microsoft HTML Clipboard Format](https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format): UTF-8、登録名、byte offsetとfragment/contextの契約。
- [Apple NSPasteboard.PasteboardType](https://developer.apple.com/documentation/appkit/nspasteboard/pasteboardtype): HTMLとplain等のOS形式。
- [html5ever 0.27.0 parse_fragment](https://docs.rs/html5ever/0.27.0/html5ever/driver/fn.parse_fragment.html): fragment parserのAPI。
