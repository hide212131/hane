# 選択テキストにクリップボードのURLをリンクとして挿入する

## 目的と成果物

本文のテキストを選択して右クリックしたとき、クリップボードがURLなら「リンクを挿入」を表示する。選ぶと、選択テキストをラベルにしたMarkdownリンクになる。

例: `Haneの紹介` を選択し、クリップボードに `https://example.com/hane` がある状態で実行すると、原文は `[Haneの紹介](https://example.com/hane)` になる。既存のリンク描画・リンクを開く操作をそのまま利用する。

この文書は実装担当向けの詳細設計案。設計PRではこの文書だけを追加する。製品実装・回帰テスト・実OS検証は未実施であり、設計PRの作成・mergeを機能完成とは扱わない。実装は別の実装用PRでClaude Codeへ引き渡す。

調査基準: main `b9676139d450a60cf51857840d0ebf64d5f20c91`（2026-10-26）。実装着手時にcurrent mainと以下の参照先を読み直す。

## 操作と表示

1. 本文でテキストを選択する。前向き・逆向きの選択、日本語・絵文字、折り返し表示を等しく扱う。
2. 本文を右クリックすると本文用コンテキストメニューを出す。既存選択がある場合は、右クリック位置へキャレットを移動せず選択を保持する。選択外の本文を右クリックした場合も同じ選択を対象とする。
3. 下記のURL条件と選択条件を満たす場合だけ「リンクを挿入」を表示する。URLが無効なときにdisabled項目だけを残さない。
4. 項目を選択すると選択範囲だけを置換し、メニューを閉じて本文へフォーカスを戻す。キャレットは挿入したリンク記法の直後へ置く。実行時にリンク先へ移動しない。
5. Escape、メニュー外クリックで閉じる。閉じるクリックを背後の本文へ伝播させて選択変更・リンク遷移を起こさない。ウインドウ端では全項目を操作できる位置へ収める。

本文用メニューには最低限「コピー」「切り取り」「貼り付け」を既存アクションへ接続して置く。選択なしではコピー・切り取りを無効にし、閲覧モードでは切り取り・貼り付けを無効にする。クリップボードにテキストがなければ貼り付けを無効にする。「リンクを挿入」はこれらと区切って配置する。入力欄・サイドバー・タブを右クリックして本文メニューを出さない。

## URLの契約

クリップボードを読むのはメニューを開くとき。GPUIの `read_from_clipboard().and_then(|item| item.text())` で得るプレーンテキストを使い、HTMLや画像からURLを推測しない。

| 入力 | 判定 |
| --- | --- |
| `https://example.com/path?q=1#part` | 有効 |
| `http://localhost:8080/`、IPアドレス、国際化ドメイン | URLとして解析可能でhostがあれば有効 |
| URL前後の空白・末尾改行 | trimしてから判定 |
| 複数URL、説明文付きURL、内部の空白・改行・制御文字 | 無効 |
| `www.example.com`、相対パス、Markdownリンク記法 | 無効。schemeを補わない |
| `javascript:`、`data:`、`file:`、`mailto:`などHTTP(S)以外 | 今回は無効 |
| 空文字列、テキストなし、取得失敗 | 無効 |

先にtrimした文字列内の空白・制御文字を検査し、URL parserが改行を除去することに依存しない。schemeは大文字小文字を区別せずHTTP(S)に限定し、`http:example.com` のような省略記法は許可せず `://` とhostを必須とする。ネットワークへ接続して存在確認しない。

workspaceにある `url` crate（調査時 `=2.5.8`）を利用し、正規表現や文字列prefixだけで判定しない。挿入する値はtrim後の入力を基準とし、hostの大小文字、queryの順序、既存のpercent encodingを不必要に正規化しない。Markdown用エスケープ以外の変換はしない。

メニューには判定済みURLを保持し、実行時にクリップボードを読み直さない。表示中に他アプリがクリップボードを書き換えても、メニューを開いたときのURLを使う。次に開いたときは新しい値を読む。クリップボード内容をログや永続設定に保存しない。

## 選択とMarkdown変換の契約

選択範囲の正本はEditorの `Selection` と `SourceRange`（UTF-8 byte offset）。描画済みテキストやUTF-16の位置から原文を作り直さない。右クリックのhit testは本文であることを確認するために使い、選択を置き換えない。

空でない、単一のインライン文脈に収まる選択をリンク化する。通常段落だけでなく見出し、リスト項目、引用内、表セル内の通常テキストにも対応する。soft wrapは原文の改行ではないため制限にしない。同一段落内のsoft line breakは保持する。

既存の太字・斜体・インラインコードなどを丸ごと含む選択では、その原文を保持して外側にリンク記法を付ける。構文の途中を横断する選択、既存リンク・画像・autolinkとの重複、コードブロック内、複数のブロックまたは表セルをまたぐ選択は、今回「リンクを挿入」を出さない。選択テキストを黙って切り詰めたり、リンクの入れ子や段落構造の変更を起こしたりしない。通常テキスト内の記号を理由に一律拒否しない。

実装は次を満たすMarkdown serializerを用意する。

- 通常のラベルとURLなら `[選択原文](URL)` を生成する。
- URL内の括弧・山括弧・backslash等がリンク終端や別の構文として解釈されないよう、CommonMarkのdestination規則に合わせてエスケープする。`&`、query、fragment、既存の `%xx` を二重encodeしない。
- ラベル内のリテラルの `[`、`]`、backslash等は必要な箇所だけエスケープする。既存の有効なinline装飾やescapeを壊す全面置換はしない。
- 元の表示文字と装飾を保ち、生成結果を既存parserで読むと意図した範囲全体が指定URLの単一リンクになることを検証する。選択外の構文解釈も壊さない。安全に生成できなければ本文を書き換えない。
- 対象範囲外のbyte列・改行コードを完全に保持する。失敗は部分編集を残さず、利用者へ短い日本語のstatusを返す。

判定と生成は `hane-markdown` の既存parser・source range情報を利用する。UIに第二のMarkdown parserや括弧数だけを数える独自構文判定を作らない。background parseが対象revisionに追いついていなければ、最新snapshotを既存parserで解析して判定し、古い結果から編集しない。必要な解析は操作時に限定し、renderごとに文書全文を走査しない。

## 編集の原子性と古いメニューの無効化

メニューのpayloadには最低限、文書identity、revision、向き込みのSelection、判定済みURLを持つ。文書identityにはsession IDだけでなく文書の置換・再読込を区別する既存の `DocumentInstance` または同等の既存generationを使う。ファイルパスやrevisionだけの一致では不十分。

項目実行直前に以下をすべて再確認する。

- 同じ文書instanceがactiveで、同じrevision・Selectionである。
- 本文が編集可能で、IME compositionがない。
- 他のmodalや入力欄が編集対象を占有しておらず、メニューが現在も有効である。
- 最新原文に対して選択・Markdown生成の条件が成立する。

タブ切替、タブclose、作業フォルダ切替、再読込、本文の編集、選択変更、閲覧モードへの切替、IME開始、ウインドウの非active化では閉じる。閉じ忘れや遅延callbackがあっても上記guardでno-opにする。別タブへ自動的に戻して編集したり、古い選択を復元して書き換えたりしない。

IME中はリンク項目を表示せず、操作が変換確定・取消を勝手に行わない。guardに失敗した場合はhistory、dirty、revision、選択、原文のどれも変えない。

リンク挿入は独立した1回のundo transactionとする。前後の通常入力とhistoryを結合しない。1回のUndoで原文と元の向き込みSelectionへ戻り、1回のRedoでリンクと実行後キャレットを復元する。現行 `EditorCommand::Insert` は前後の入力とgroupingされ得るため、その呼び出しだけで完了としない。既存の `Editor::replace_range_recorded(range, replacement, selection_after)` を利用する。このAPIが持つ独立したhistory境界とSelection復元を使い、リンク専用の置換・history APIを重複追加しない。UIからbufferを直接編集しない。IMEをcommitするAPIなので、呼ぶ前にcompositionなしを確認する。

成功した `EditSummary` は既存の `EditorView::after_input` に接続する。dirty、保存・recovery、再解析、表示cache、find結果、選択描画は既存の編集経路と同じ更新を受ける。保存して再読込したときも同じMarkdown原文とリンク先を保持する。

## 実装担当が読む場所と変更責務

| 場所 | 現状と今回の責務 |
| --- | --- |
| `crates/ui/src/view/render.rs` | 本文と行に左クリックhandlerがある。本文右クリック入口と最前面のメニュー描画を追加する |
| `crates/ui/src/view.rs` | `on_row_mouse_down` は選択・リンクを開く処理を行うため右クリックから呼ばない。`file_tab_context_menu_*` の表示部品を参考にする。本文メニューの詳細は新しい `view/editor_context_menu.rs` 等へ分離する |
| `crates/ui/src/view/input_routing.rs`、`actions.rs` | コピー・切り取り・貼り付けの現在のroutingを使い、他の入力欄へ誤配送しない。Escapeの優先順位へ本文メニューを加える |
| `crates/ui/src/view/reading_mode.rs` | `body_editable` と `command_changes_text` による防御を保つ。新しい編集commandを足すなら分類を更新する |
| `crates/editor/src/lib.rs`、`history.rs`、`selection.rs` | 既存 `replace_range_recorded` とそのhistory境界・Selection復元を再利用する。リンク専用の置換APIを追加しない。URLのclipboard取得やメニュー状態は持たせない |
| `crates/markdown/src/syntax.rs` と関連module | 既存 `LinkParse`、`links_intersecting`、block/inline source rangeを利用した適用可否とリンク生成。構文判定・エスケープの単体テストを置く |
| `crates/session/src/session.rs` と既存UI document identity | 文書instance/revisionを参照する。新たな永続的メニュー状態は作らない |

具体的なAPI名は実装時のcurrent mainに合わせる。既存ファイルの大規模移動、parserの置換、通常貼り付けの仕様変更を混ぜない。

## 回帰テストと受入条件

以下は実装PRの完了条件。設計PRのチェック結果で実装済みとしない。

| ID | 操作・境界 | 必要な結果・証拠 |
| --- | --- | --- |
| A1 | HTTP/HTTPSをコピー→本文選択→右クリック→リンクを挿入 | 項目が出る。ラベル・URL・選択外原文が正しく、実行でブラウザを開かない。GPUIイベントテストと実OS確認 |
| A2 | URL判定表の全分類、空clipboard、HTML/画像のみ | 有効時だけ項目が出る。ネットワーク取得なし。純粋関数テストとGPUIテスト |
| A3 | 日本語、絵文字、結合文字、逆向き選択、soft wrap、文書先頭・末尾 | byte境界を壊さず対象選択だけリンクになる。EditorとGPUIテスト |
| A4 | ラベルの括弧・backslash・装飾、URLの括弧・query・fragment・percent encoding・国際化ドメイン | parserでラベルの表示・装飾と指定destinationを確認。保存・再読込でも保持 |
| A5 | 見出し・リスト・引用・表セル、段落内改行、既存リンク、構文途中、code block、複数block/cell | 対応文脈は正しく生成。非対応範囲では項目なし、原文無変更。parserテスト |
| A6 | 直前に入力→挿入→直後に入力→Undo/Redo | 挿入前後と別transaction。原文・Selection・キャレット・dirty状態が既存契約どおり復元。Editorテスト |
| A7 | 表示中にタブ/フォルダ切替・close・再読込・編集・選択変更。遅延callbackも実行 | 別文書・古い選択を編集しない。identity/revision/selection guardのGPUIテスト |
| A8 | 閲覧モード、IME中、入力欄の選択、modal表示中 | 本文リンク挿入は起きず、未確定文字と他入力欄の状態を保持。GPUIテストと実IME確認 |
| A9 | 開いた後にclipboard変更、Escape、外側クリック、右/下端、他メニューとの切替 | snapshot URL使用、選択保持、背後へclick漏れなし、項目を操作可能。同時表示なし。GPUIテストと実OS確認 |
| A10 | 通常のcopy/cut/paste、挿入後の編集・保存・再読込・既存リンクを開く操作 | 既存機能が維持され、新しいリンクが既存描画・操作へ接続される。focused regressionと実OS確認 |

実装PRでは変更に対応する `hane-editor`、`hane-markdown` のテストとfocused GPUIテスト、既存CIのbuild/test/lintを実行する。macOSとWindowsの実際のclipboardでA1、A8〜A10と保存・再読込を確認し、対象head、OS、操作、観測結果を残す。Windows検証はWindows環境から直接行う。未実施・失敗・環境依存で未確認の項目をpassと書かない。

## 関連作業と引き渡し

調査時点で同目的のopen PRはない。#596（書式付きclipboard）、#598（ファイルdropによるリンク挿入）、#603（閲覧モードのリンク操作）、#614 / #615（リンクcursor）は関連するが本機能の代替ではない。通常貼り付け・ファイルリンク・リンクcursorの実装を本PRへ混ぜない。

open PR #622（本文置換）は選択置換・history・入力後処理と重なる。再確認時のheadは `901eb21c897983e3da4a9939eda30ac195df714c`。置換planは既存 `replace_range_recorded` を使っており、本機能も同じAPIを使う。実装着手時にcurrent diffと採用済みAPIを確認し、原子的置換の重複APIを作らない。#626（タスクcheckbox、再確認時head `f8ec7dfc04f946e275b7f98898fb6fab38b9f3fc`）も `replace_range_recorded` と `after_input` を使う。本文クリックと重なるため、Left限定guardを保ち、右クリックをcheckbox toggleへ伝播させない。#620（設定画面分離）と#625（network core）は調査時点で本機能の直接の前提ではない。全域refactor #297完了は待たず、共有契約に実際の競合があるときだけ順序を調整する。

設計採用後はこのファイルを実装branchから読める状態にして、別の実装用Draft PRへ引き渡す。依頼にはこのパス、対象head、A1〜A10、今回の対象外を含める。製品コードと直接関連するテストはClaude Codeが担当し、起動と結果確認はdefault branch上の `docs/aadw-claude-execution.md` とCommander Policyに従う。設計用PRへ実装起動コメントを投稿して転用しない。

2026-10-26再確認: current main `b3dd52e22061bc79305fbf80082b4821a887dd94` の既存編集APIと上記関連headを照合した。調査基準以後のmain差分はnetwork coreとCargo依存であり、本設計の本文入力・history契約を変更していない。#622 / #626はこの時点では未merge。実装開始時には再びcurrent main/headを確認する。
