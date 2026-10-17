# Issue #494: OS標準のキーによる基本編集操作

- 仕事の契約: [Issue #494](https://github.com/hide212131/hane/issues/494)
- 基準main: `2164b895e849eef4de8c80c3696c0fd8e00c496b`
- 実装branch: `work/issue-494-basic-editing`、PR #495
- 2026-10-17: 利用者の追加指定により初回仕様を改訂。Emacs配列は外部のKarabiner/Keyhac等で実現する。
- この改訂commitは文書のみ。製品コード、CI、レビュー、native GUIの完了はPRのcurrent evidenceで確認する。
- 旧依頼run `38985021618` の製品差分はfinalizerのpause検査でpush前に停止した。旧依頼を継続せず、この文書を正として新規実装する。

## 1. キー割当

**HaneにはEmacs用のControl英字の別名を追加しない。** 各OSの通常のキーで編集操作を提供する。OSに共通の標準がない操作には独自キーを発明しない。

| 操作 | macOS | Windows / 非macOS |
| --- | --- | --- |
| 全選択 | Command+A（維持） | Ctrl+A（維持） |
| 左右・上下移動／選択 | 矢印、Shift+矢印（維持） | 矢印、Shift+矢印（維持） |
| 行頭／行末へ移動 | Command+←/→（追加） | Home/End（維持） |
| 行頭／行末まで選択 | Command+Shift+←/→（追加） | Shift+Home/End（維持） |
| 文書先頭／末尾へ移動 | Command+↑/↓（維持） | Ctrl+Home/End（維持） |
| 文書先頭／末尾まで選択 | Command+Shift+↑/↓、Shift+Home/End | Ctrl+Shift+Home/End |
| 単語移動／選択 | Option+←/→、Shift付き（追加） | Ctrl+←/→、Shift付き（追加） |
| 段落移動／選択 | Option+↑/↓、Shift付き（追加） | Ctrl+↑/↓、Shift付き（修正） |
| 前の単語を削除 | Option+Backspace（追加） | Ctrl+Backspace（追加） |
| 後の単語を削除 | Option+Delete（追加） | Ctrl+Delete（追加） |
| 1画面分カーソル移動 | Option+PageUp/PageDown（追加） | PageUp/PageDown（追加） |
| 1画面分の選択 | 今回は新規割当なし | Shift+PageUp/PageDown（追加） |
| 文書先頭／末尾を表示 | Home/End（既存のカーソル移動から変更） | 独自キーを作らない |
| 1画面分スクロール | PageUp/PageDown（追加） | 独自キーを作らない |
| カーソル行を画面中央に表示 | Control+L（既存のAI起動から変更） | 独自キーを作らない |
| 文字／選択の削除 | Backspace/Delete（維持） | Backspace/Delete（維持） |
| コピー／切り取り／貼り付け | Command+C/X/V（維持） | Ctrl+C/X/V（維持） |
| Undo／Redo | Command+Z、Command+Shift+Z（維持） | Ctrl+Z、Ctrl+Y、既存Ctrl+Shift+Z（維持） |

Backspaceは後方削除、Deleteは前方削除のGPUI内部名。Macの刻印deleteはBackspace相当で、前方削除はFn+Delete等から使う。Home/Endは小型MacでFn+左右、PageUp/DownはFn+上下から使われるが、HaneはGPUIが受け取るキーへ登録する。

初回仕様のControl+A/E/B/F/P/N/K/Y/H/DとShift版、Windows Ctrl+Shift+Backspace/Delete、Command+Backspace/Deleteは追加しない。Control+Kのkill、Control+Yのyank、kill ring、Emacs設定は作らない。行単位の編集はShift+行頭末移動で選択し、通常の削除／切り取りを使う。Windows Ctrl+Yを貼り付けへ変更しない。OSのControl+SpaceやOption/Alt+英字による文字入力を奪わない。

既存の検索、保存、開く、ファイルタブ等のアプリ操作は維持する。Windows Ctrl+Insert / Shift+Insert / Shift+Delete等の補助別名は今回は必須にしない。

## 2. 行・単語・段落の到達位置

### 行頭末

キーの組合せは各OSの一般的な編集操作に合わせる。行頭・行末はHaneの既存仕様を維持し、改行文字で区切られた原文の行の先頭・末尾を指す。画面幅による折り返しはこの行の区切りに含めない。

既存の `MoveToLineStart/End` と `line_content_range` を再利用する。表示行末のcaret affinity、表セルのfragment境界、Selectionの形の変更は今回の範囲に含めない。READMEにもOS標準テキスト欄との完全互換とは書かない。

### 単語

既存の `unicode-segmentation` を利用する。ASCII空白だけで分割しない。単語途中からはその単語の対応する端へ進み、空白・句読点は必要な方向へ越える。記号・絵文字だけでも移動が止まり続けないようにする。日本語辞書や新しい依存は追加しない。

`one two` のbyte位置:

| 操作 | 到達 |
| --- | --- |
| Mac前方 | 0 → 3 → 7 |
| Windows前方 | 0 → 4 → 7 |
| 後方（両OS） | 7 → 4 → 0 |

`one, two` はMac前方0→3→8、Windows前方0→5→8、後方8→5→0。単語の途中・行末・空行からも進み、CRLFの中間、結合文字や家族絵文字の途中へ置かない。

既存の左右移動との一貫性を保ち、非Shift単語移動は選択があれば方向側の端へ畳むだけとし、同時に追加の一語へ進まない。Shift付きはanchorを保ちactiveだけを動かす。単語削除の範囲は各OSの対応する移動の到達位置までとする。選択があれば、その選択だけを削除する。

### 段落

今回の段落は原文の改行区切りとする。Markdownの空行区切りブロックではない。LF/CRLF/CRを認識し、改行の内部へカーソルを置かない。

- 後方は両OSとも現在の段落先頭へ、既に先頭なら前段落の先頭へ進む。
- Windows前方は次の段落先頭へ進む。
- Mac前方は現在の段落末尾へ、既に末尾なら次段落の末尾へ進む。
- 空行・連続空行でも一回の操作が進むか文書端で停止し、ループしない。
- Shift版はanchorを保ちactiveだけを同じ規則で移動する。

例 `one\ntwo\nlast` のbyte位置は、後方の9→8→4→0、Windows前方0→4→8→12、Mac前方0→3→7→12を確認する。非Shift時の選択の扱いは単語移動と同様、まず方向側の端へ畳む。

単語／段落操作のたびに `full_text()` で文書全体を複製しない。既存の行／範囲APIを使い必要な方向へ読む。巨大な一行の追加コストは、残る場合は明記する。

## 3. 編集履歴と入力先

単語削除は一回の編集として、既存の `replace_range_recorded()` 等から適用する。元のselectionを記録し、Undo/Redoで本文とselectionが戻るようにする。見かけの選択を広げてから通常Deleteへ渡し、Undoで元のselectionを失う実装は避ける。通常の文字削除と意図せず履歴結合しない。単語削除はOS clipboardを変更しない。空の削除は履歴とclipboardを汚さない。

新規操作は受入済みRF3-Aの `input_target()` を使い、本文に限定する。検索入力／結果、文書内検索入力、絞り込み、rename、設定、AI popupに入力中に本文へ漏らさない。新しいguardは既存経路を利用して重複を抑える。既存の入力先別動作を壊さない。

日本語変換中は新しい本文操作で勝手にcommitして移動／削除しない。`Editor::dispatch()` はcompositionをcommitするため、UI入口で必要なguardを通す。既存ネイティブcallback、入力先の優先順、IMEプロトコル全体は作り直さない。

## 4. ページ移動とスクロール

カーソルのページ移動は実際の本文 `viewport_height` とレイアウトを使い、少なくとも1表示行を動かせる高さにする。見出し等の行高、折り返し、横位置保持を尊重する。前後の一行程度が重なる表示でよい。WindowsのShift版は同じ到達点まで選択する。

固定20行など原文の行数反復で代用しない。ページ操作で `after_input()` を繰り返さない。高さ索引、必要な範囲のレイアウト、既存の仮想表示を利用し、文書全体を同期整形しない。表の隠れたdelimiterを表示行に数えず、文書端／極小viewportで必ず停止する。移動後は既存のカーソル追従と、記号表示切替後の位置確認を通す。

MacのHome/End/PageUp/PageDownとControl+Lはスクロールだけを変える。本文、selection、Undo履歴は不変。`after_input()` や無条件のcaret revealで即座に元の場所へ戻さない。`pending_caret_visibility_after_layout` と慣性スクロールを必要に応じて解除する。Control+Lは現在のカーソル行を可能な範囲で縦中央へ置き、文書頭末のscroll範囲にclampする。

## 5. 選択AIの入口を保持する

macOSのControl+LをAI起動から中央表示へ変更する。選択AIへ到達できなくしないため、既存 `EditorView::footer_element` の操作行へ小さい「AIに指示」ボタンを追加する。単独ファイル時の「設定」の後、Autosaveの前の既存行を使える。既存 `Button` の `xsmall().ghost()`、説明「選択した文章に指示する」を使い、新しいtoolbarやfooterの高さ変更はしない。両OSで表示する。

クリックから既存 `open_ai_selection(window, cx)` を呼び、本文selectionを変更・再取得する別の仕組みは作らない。footerは本文行の兄弟なので、本文mouse handlerを呼ばない。未選択時の案内、設定不足、入力先・IMEの抑止は既存入口を再利用する。AI popupが開いているときは無効にし、同クリックによる閉じて開き直す動作を避ける。

WindowsのAlt+Iはアプリ固有のAI操作として維持する。`docs/ai-selection-actions-implementation-spec.md` の現行キーと確認手順をボタン起動へ更新し、過去の実装記録は過去の事実として区別する。READMEの通常キー表へAI固有キーを混ぜてOS標準と説明しない。

## 6. 実装箇所と検証

主な読み取り先:

- `crates/editor/src/lib.rs`、`movement.rs`、`history.rs`: command、dispatch、境界、履歴。
- `crates/ui/src/actions.rs`: OS別bindingとguard。既存 `secondary-up/down` を共通の文書移動に使うとWindows Ctrl+↑/↓が衝突するため分ける。Mac Home/EndとShift版も同じactionに一括登録しない。
- `crates/ui/src/view/input_routing.rs`: 受入済み入力経路。
- `crates/ui/src/view.rs` の `move_vertical`、`layout_around`、`after_input`、`footer_element`、AI入口とカーソル追従。
- `crates/ui/src/view/viewport.rs`: viewport/scroll。
- `crates/editor/tests/cursor_boundaries.rs`、`editor_scenarios.rs`、`ime_contract.rs`、既存UIテスト。

E01〜E06に直接対応する意味のあるcore/UIテストを追加する。登録キーを実際にdispatchし、到達位置へ文字を挿入して確認するテストも必要。Windows Ctrl+Aは選択を置換でき、Ctrl+↑/↓は文書移動にならないこと、Mac Home/Pageはselectionが変わらないことを確認する。両OS条件のkeymapを取り違えない。

ASCII/Japanese混在、結合文字、家族絵文字、LF/CRLF/CR、前向き／逆向き選択、空行、文書端、Undo/Redo、狭いviewportでの折り返し、表／リスト／コードの既存保護を含める。スクロール専用操作の履歴・selection不変、AIボタンの実クリックと選択保持、変換中の本文guardを確認する。キーが同じactionへ登録されているだけのテストで意味を代替しない。

native GUIでの重点確認:

| 組 | 操作と観測 |
| --- | --- |
| 基本移動・選択 | `one two\n日本語 é 👨‍👩‍👧‍👦\nlast` でOS別の行頭末・単語・段落・文書頭末とShift版を送り、挿入位置・選択範囲を確認 |
| 削除・復元 | 単語削除、行端まで選択→通常削除／切り取り→貼付、Undo/Redo、保存bytesを確認 |
| 表示 | 狭い長文、見出し、表、コードでカーソルのページ移動とMacスクロール、Control+Lを使い、caret/selectionとviewportを区別して確認 |
| 入力先・日本語 | 検索、絞込、rename、AI、設定、OS日本語変換中で本文誤操作・二重入力がないことを確認 |
| AI入口 | 本文選択→ボタン実クリックで同じ選択のAI画面を開けること、Windows Alt+I維持を確認 |

現行のHosted総合GUI `hosted-gui-interaction/7` は既存操作の回帰確認には使えるが、新しい単語／段落／ページキーやAIボタンを送るシナリオではない。Windowsのnative GUIは別のWindows操作環境が必要。CI、GPUIテスト、既存GUIの起動・撮影だけを新キーのnative確認として数えない。

通常workerは製品コードと直接関係するテスト・README・上記AI仕様の現行記述だけを編集する。workflow、vendor、AGENTS、Commander Policy、AI基盤、Selectionの形、入力経路全体の変更はしない。shell/test/build/lint/commit/pushは使わない。実行できない検証を完了と書かない。trusted finalizer後にverify、macOS/Windows CI、current-headのCodeRabbit manual full review、必要なnative GUIを確認する。

## 7. 参考

- [Microsoft: Keyboard shortcuts in Windows / Text editing](https://support.microsoft.com/en-gb/accessibility/windows/keyboard-shortcuts-in-windows)
- [Apple: Mac keyboard shortcuts / Document shortcuts](https://support.apple.com/en-us/102650)
- [Apple Pages: Keyboard shortcuts / Move around and select text](https://support.apple.com/guide/pages/keyboard-shortcuts-tanc0ffef022/mac)

OS標準一覧とアプリ固有の補足を区別する。Option+PageUp/DownとShift+Home/Endの具体的な動作はApple Pagesの公開表を参照した。Mac Shift+PageUp/Downのキー対応は取得した公式表から確定できないため、今回新規に決め打ちしない。
