# Issue #494: 基本編集操作の実装メモ

- 仕事の契約: [Issue #494](https://github.com/hide212131/hane/issues/494)
- 基準main: `2164b895e849eef4de8c80c3696c0fd8e00c496b`
- 実装branch: `work/issue-494-basic-editing`
- 準備時点: 文書だけ。製品実装、CI、レビュー、実画面の検証は未実施。
- このファイルは具体的なキー、例、接続先をまとめる。目的・範囲・受入条件の正本はIssue。将来の状態はcurrent PRの証拠を確認する。

## 1. キーと実装の対応

既存の一般的な操作を残し、本文の基本操作を補う。全キーをEmacsに切り替える設定は作らない。

| 操作 | macOS | Windows / 非macOS |
| --- | --- | --- |
| 行頭 / 行末へ移動 | 既存Home/End、追加Command+←/→、追加Control+A/E | 既存Home/End |
| 行頭 / 行末まで選択 | 既存Shift+Home/End、追加Command+Shift+←/→、追加Control+Shift+A/E | 既存Shift+Home/End |
| 文書の先頭 / 末尾へ移動・選択 | 既存Command+↑/↓、Shift付き | 既存Ctrl+Home/End、Shift付き |
| 1文字前 / 後 | 既存←/→、追加Control+B/F | 既存←/→ |
| 1表示行上 / 下 | 既存↑/↓、追加Control+P/N | 既存↑/↓ |
| 上記の移動で選択を延ばす | 既存Shift+矢印、追加Control+Shift+B/F/P/N | 既存Shift+矢印 |
| 前の単語先頭 | 追加Option+← | 追加Ctrl+← |
| 次の単語 | 追加Option+→（単語末尾） | 追加Ctrl+→（次の単語先頭） |
| 単語ごとに選択を延ばす | 上記Option+左右にShiftを追加 | 上記Ctrl+左右にShiftを追加 |
| 前の単語を削除 | 追加Option+Backspace | 追加Ctrl+Backspace |
| 後の単語を削除 | 追加Option+Delete | 追加Ctrl+Delete |
| 行頭まで削除 | 追加Command+Backspace | 追加Ctrl+Shift+Backspace |
| 行末まで削除 | 追加Command+Delete | 追加Ctrl+Shift+Delete |
| 行末まで切り取り | 追加Control+K | 追加Ctrl+K |
| 切り取った内容を貼り付け | 追加Control+Y、既存Command+V | 既存Ctrl+V（Ctrl+YはRedoのまま） |
| 1画面分上 / 下へ移動 | 追加PageUp/PageDown | 追加PageUp/PageDown |
| 1画面分選択を延ばす | 追加Shift+PageUp/PageDown | 追加Shift+PageUp/PageDown |
| 前 / 後の1文字削除 | 既存Backspace/Delete、追加Control+H/D | 既存Backspace/Delete |

表のBackspaceは後方削除、Deleteは前方削除というGPUIのキー名を示す。Macのキーボードで「delete」と刻印された後方削除はBackspaceに対応し、前方削除はFn+Delete等で送信される。

文書全選択、コピー、通常切り取り、保存、開く、Undo/Redo、文書内検索と検索結果移動は既存実装を維持する。Control+HはmacOSの後方削除を参考にする。GNU Emacs標準ではHelpなので「Emacs完全互換」と案内しない。

既存macOS Control+LのAI操作、Windows Ctrl+A/F/S/O/V/Y、OSのControl+Spaceを上書きしない。Option/Alt+英字を一括登録して文字入力を奪わない。

## 2. 単語の到達位置

既存の `unicode-segmentation` を使い、ASCII空白だけに依存しない。単語の途中で開始した場合はその単語の対応する端へ進む。空白と句読点は次の単語まで越え、絵文字等の記号しかない範囲でも移動が止まり続けないよう、分断しない境界を使う。日本語の辞書や新しい依存は追加しない。

`one two` のbyte位置は次の例をテストで固定する。

| 操作 | 開始 | 到達 |
| --- | --- | --- |
| macOS前方 | 0 | 3 |
| macOS前方 | 3 | 7 |
| Windows前方 | 0 | 4 |
| Windows前方 | 4 | 7 |
| 後方（両OS） | 7 | 4 |
| 後方（両OS） | 4 | 0 |

`one, two` はmacOS前方0→3→8、Windows前方0→5→8、後方8→5→0を確認する。空行・行末から隣の行へも移動できるようにする。CRLFを分割した位置やUnicode文字の途中へは移動しない。単語削除の範囲は対応する移動の到達位置までとする。

選択があるときの非Shift単語移動は、既存の左右移動と同様、方向に応じた選択端へ畳む。その操作でさらに次の単語へ進まない。Shift付きはanchorを保ちactiveだけを動かす。削除時に選択があれば選択範囲だけを対象にする。

単語操作のたびに `full_text()` で文書全体を複製しない。既存の行/範囲APIを利用し、必要な方向・境界まで読む。長い一行への追加の性能リスクがあれば明記する。

## 3. 行を削除・切り取る

行は `line_content_range` が表す原文の行であり、折り返した画面の一行や表のセルの端へ意味を変更しない。

行頭/行末までの通常削除は改行を含めず、既に境界上なら何もしない。切り取りではなく削除なのでOS clipboardを変更しない。

Control+Kは次の範囲を一回の編集で切り取る。

| 状態 | 切り取る範囲 |
| --- | --- |
| 選択あり | その範囲だけ |
| 行途中に本文が残る | 現在位置から改行直前 |
| 行末、または残りがASCII空白/タブだけ | 残る空白/タブと、その行の改行まで |
| 最終行で改行なし | 残る文字だけ |
| 文書末尾、対象が空 | 何もしない |

切り取る内容を検証して、成功した削除と対応する文字列をOS clipboardへ置く。空の操作や失敗で既存clipboardを消さない。CRLFは二文字を一つの改行として扱う。通常PasteとmacOS Control+Yはこのclipboardを使う。

今回は直近1件の通常clipboardを使い、連続Control+Kの合算と複数履歴の循環は実装しない。この差をREADMEに書く。各切り取り/削除は一回のUndoで元の本文・選択に戻り、Redoで同じ編集を再現する。既存の文字削除との履歴結合に巻き込まない。

## 4. ページ移動と画面

`PageUp/PageDown` は本文の表示高さに応じて1画面分の移動を行う。少なくとも1表示行を動かせる高さにし、前後の一行程度が重なる表示でよい。Shift版は同じ到達点まで選択する。

固定20行などにはせず、`viewport_height`、実レイアウト、既存の横位置保持を利用する。折り返しと見出し等の行高を尊重する。`EditorCommand::MoveUp/Down` を原文の行数だけ反復して代用しない。画面単位の処理を一つにまとめ、文字ごと/行ごとに `after_input()` を繰り返さない。既存の見える範囲だけを描く方式と高さ索引を使い、文書全体を同期的に整形・解析しない。

文書頭末・極小viewportで必ず停止する。表の隠れたdelimiterを表示行に数えない。移動後は既存のカーソル追従と、記号の表示が切り替わった後の位置確認を通す。

## 5. 入力先と編集履歴の接続

受入済みRF3-Aの `input_target()` を使う。新規操作は本文だけに接続し、設定、検索入力/結果、絞り込み、rename、AI popupで本文へ漏らさない。既存の単位を持つキー別名を既存actionへ登録する場合、既存の入力先別処理を使い、既存の入力経路を壊さない。

日本語変換中は新しい本文操作で勝手にcommitして移動/削除しない。`Editor::dispatch()` はcompositionをcommitするため、必要なguardをUI入口で設ける。既存ネイティブcallbackや入力先の優先順を作り直さない。

新たな削除は編集前のselectionを保存し、既存の `replace_range_recorded()` 等の正規履歴経路で適用する。見かけだけ選択を変えて通常Deleteへ渡し、Undo時に元のselectionが失われる実装を避ける。

主な読み取り先:

- `crates/editor/src/lib.rs`: command、dispatch、`replace_range_recorded`。
- `crates/editor/src/movement.rs`: grapheme、行境界、既存削除。
- `crates/editor/src/history.rs`: 編集のまとまり。
- `crates/ui/src/actions.rs`: platform bindingsと入力先guard。
- `crates/ui/src/view/input_routing.rs`: 受入済みの入力先。
- `crates/ui/src/view.rs`: `move_vertical`、`layout_around`、`after_input`、カーソル追従。
- `crates/ui/src/view/viewport.rs`: viewport/scrollの既存処理。
- `crates/editor/tests/cursor_boundaries.rs`、`editor_scenarios.rs`、`ime_contract.rs`と既存UIテスト。

今回のためにUI全体を整理したり、保護パス・vendor・workflowを変更しない。新しいsmall moduleが自然なら製品範囲内で切り出してよい。

## 6. 検証の準備

Issue E01〜E06に対応するcore/UIテストを追加する。単にenumを直接呼ぶだけではなく、登録キーをdispatchし、編集後に文字を挿入して到達位置を確かめる。ASCII/Japanese混在、結合文字、家族絵文字、CRLF/CR、前向き/逆向き選択、空行、折り返し、表/リスト/コードの保護を含める。

実画面でのfocused確認は次の5組を使える。

| 組 | 操作と観測 |
| --- | --- |
| 移動・選択 | `one two\n日本語 é 👨‍👩‍👧‍👦\nlast`を開き、各OSの行頭末・単語・文書頭末キーとShift版を送り、挿入位置/選択範囲を確認する |
| 削除・復元 | 単語/行頭末を削除しUndo/Redo。Control+K→貼り付け、行末Control+K、空の末尾Control+Kを確認し、保存したbytesを比較する |
| 表示 | 幅を狭くした長文、見出し、表、コード、画像の近くでPageUp/DownとShift版を使い、横位置・選択・画面内のcaretを確認する |
| 入力先 | 検索欄/結果/絞り込み/rename/AI/設定で新規キーを送り、本文とclipboardが誤変更されないことを確認する |
| 日本語入力 | 実OSの日本語変換中に新しいControl/単語/行編集キーを使い、変換・確定・取消と本文への二重入力がないことを確認する |

現行のHosted総合GUI `hosted-gui-interaction/7` は既存操作の回帰確認には使えるが、新しい単語/行/ページキーを直接送るシナリオではない。Windowsの実画面は別のWindows操作環境が必要。これらを実施済みとしない。CI成功、GPUIのテスト、起動と撮影のみの結果は、未実施のnative確認を置き換えない。

通常workerは製品コードと直接関係するテスト・READMEのみを編集する。shell/test/build/lint/commit/pushは使わない。trusted finalizer後のverifyと通常CIをCommanderが確認し、必要なCodeRabbitレビューと実画面の証拠をそろえる。準備PRやコードpushだけでIssueをcloseしない。

## 7. 参考

- [GNU Emacs: Moving Point](https://www.gnu.org/software/emacs/manual/html_node/emacs/Moving-Point.html)
- [GNU Emacs: Words](https://www.gnu.org/software/emacs/manual/html_node/emacs/Words.html)
- [GNU Emacs: Killing by Lines](https://www.gnu.org/software/emacs/manual/html_node/emacs/Killing-by-Lines.html)
- [Apple: Mac keyboard shortcuts](https://support.apple.com/en-us/102650)
