# Windows P3 実画面確認

- PR: [#491](https://github.com/hide212131/hane/pull/491)
- 実画面確認の対象コードcommit: `e6c6e3e26fea621bafcb60cc3b04d656c33a2194`
- 確認開始時と証拠記録前のPR headはいずれも上記SHA。後続commitは検証記録と画面証拠のみ。
- 目的: Windows P3の不足している手動検証証拠。設計・製品コードの変更なし。

## 実行環境

- Windows 11 Home 10.0.26200 (build 26200)、ARM64（WindowsのCPU情報: Apple silicon）。
- Rust stable `1.95.0 (59807616e 2026-04-14)`、target `aarch64-pc-windows-msvc`。
- Japanese (Japan) Microsoft IME、表示スケール 200%。アプリウィンドウは 962×790 px。
- `cargo build --release --locked -p hane` 成功。自動テストは実行していない。
- binary: isolated worktree の `target/release/hane.exe`。
- binary SHA-256: `43D6C1A0926D2AFB1177E20A14BBCB0F09B137184FE3896807C9251AAFF29898`。
- fixture文書と `HANE_STATE_DIR` は検証用worktree配下に分離。利用者の文書・設定は読み書きしていない。

## 操作と結果

| 項目 | 実操作と結果 | 判定 |
|---|---|---|
| 本文の日本語IME | Microsoft IMEで `nihongo` → Space → Enter。候補が本文カーソルの近くに表示され、日本語を確定。空行上で `ka` → Escape は未確定文字を残さなかった。 | 一部確認。確定済み日本語を選択して `ka` 変換中にEscapeすると、選択前の日本語も消えた。選択維持が必要なcancel経路は不合格。 |
| Filterの日本語IME | Filterに `kanji` → Space → Enter、`感じ`を確定。ファイル一覧が絞られ本文は変化しない。 | 一部確認。選択済み `感じ` の上で `ka` → Escapeすると元の選択文字が消え、Filterが空になった。 |
| 名前変更欄のIME | inline renameで `ka` を入力し候補を表示。Escape 1回でIME compositionを取り消し、2回目でrename UIを閉じ、実ファイル名 `rename-source.md` は不変。別操作で変換を確定した後もeditorが開いた。 | 一部確認。変換中に選択していた元の名前が欄から消えた。rename自体はEscapeで取消されファイル名は保持。 |
| rename処理待ち中の入力保護 | renameを実行し、一時名 `ぺ.md` への変更と元の `rename-source.md` への復帰を画面で確認。 | 未検証。ローカルのrenameが画面で制御できるpending状態より速く完了し、処理待ち中の入力保護状態を再現できなかった。 |
| 本文の選択置換・Clipboard | 日本語行を選択してCopy/Cut/Pasteで復元。`Unicode: A😀éZ`（`e` + U+0301）行も選択Copy/Cut/Pasteし、Cutで行が消え、Pasteで同じ行が戻ることを確認。Unicode行を選択した日本語行へPasteして置換し、Undoで元に戻した。`BODY-ANCHOR`は全段階で残った。 | 確認 |
| FilterのClipboard | 本文の日本語行をCopyしFilterへPaste。Filter内でもCopy/Cut/Pasteを実施し、本文側は変化しなかった。 | 確認 |
| コンテンツ検索 | 左の本文検索で `search` を入力し、`manual.md` の `SEARCH-ANCHOR` を含む1文書の結果を表示。Enterで本文へ文字が漏れず、Tabは結果行へ移動、Escapeで通常のファイル一覧へ戻った。 | 確認。検索画面の個別スクリーンショットは保存していない。 |
| 文書内検索 | Ctrl+FでFind欄を開き `BODY-ANCHOR` を検索、1/1件を表示。Enterで先頭へ戻る表示。Tabでは本文に入力されない。Escapeで左ペインはファイル一覧に戻ったがFindバーは残り、「閉じる」操作で終了。 | 確認。EscapeだけではFindバーが閉じない挙動を記録。 |
| AI指示画面 | 本文選択中にAlt+I。接続なしのため送信ボタンはdisabled。Enterで送信されず、Tabはdisabledボタンへ移動、Escapeで選択本文へ復帰。 | 確認。AIへ送信していない。 |
| 本文Undo/Redo・保存・再読込 | `保存確認-ORIGINAL` に `-EDITED` を入力。Ctrl+Zで戻し、Ctrl+Shift+Zで再適用。Ctrl+S後に `Saved`、fixtureファイル上の保存値を確認。タブを閉じ、サイドバーから再度開き、`-EDITED` が表示された。 | 確認 |

## 読み取り上の注意

- IMEのEsc取消で選択前の文字が失われる挙動が、本文・Filter・rename欄で観測された。これらは成功扱いにしていない。
- 名前変更pending保護は未検証。自動テストや単体テスト結果で代替していない。
- AIへの送信は行っていない。AI接続が設定されておらず送信ボタンは無効。
- 再読込・Unicode操作時の画像には、操作途中に一時作成して後で削除した空の `New Folder` 行が残るものがある。証拠採取後、検証用fixtureから削除し、fixtureには `manual.md` と `rename-source.md` のみが残ることを確認した。
- PR #491はopen、Issue #304もopenのまま。macOSの残る必須確認が揃うまでmerge/closeしない。

## 画面証拠

各画像は当該画面のキャプチャです。3枚目の本文Esc取消画面は、空行での取消後画面と同一でした。

### 本文：IME変換中、候補ウィンドウが本文カーソル付近に表示

![本文：IME変換中、候補ウィンドウが本文カーソル付近に表示](screenshots/01-body-ime-composing.jpg)

### 本文：日本語変換を確定した状態

![本文：日本語変換を確定した状態](screenshots/02-body-ime-committed.jpg)

### 本文：IME取消後。選択前の日本語が残らず、空になった行

![本文：IME取消後。選択前の日本語が残らず、空になった行](screenshots/03-ime-escape-after-selection.jpg)

### Filter：IME変換中

![Filter：IME変換中](screenshots/05-filter-ime-composing.jpg)

### Filter：日本語変換を確定

![Filter：日本語変換を確定](screenshots/06-filter-ime-committed.jpg)

### Filter：選択文字上でIME取消後、元の文字が消えた状態

![Filter：選択文字上でIME取消後、元の文字が消えた状態](screenshots/07-filter-ime-cancel.jpg)

### Filter：本文からコピーした文字列の貼り付け

![Filter：本文からコピーした文字列の貼り付け](screenshots/08-filter-clipboard-paste.jpg)

### 名前変更欄：IME取消とrename取消

![名前変更欄：IME取消とrename取消](screenshots/09-rename-ime-cancel.jpg)

### 名前変更欄：IME確定後、rename editorが開いた状態

![名前変更欄：IME確定後、rename editorが開いた状態](screenshots/10-rename-ime-committed.jpg)

### AI指示画面：接続なしで送信無効

![AI指示画面：接続なしで送信無効](screenshots/11-ai-open-disabled-send.jpg)

### AI指示画面：Tabで無効な送信ボタンへ移動

![AI指示画面：Tabで無効な送信ボタンへ移動](screenshots/12-ai-tab-focused-send-disabled.jpg)

### AI指示画面：Escapeで本文へ復帰

![AI指示画面：Escapeで本文へ復帰](screenshots/13-ai-escape-return-body.jpg)

### 文書内検索欄を開いた状態

![文書内検索欄を開いた状態](screenshots/14-document-find-open.png)

### 文書内検索でBODY-ANCHORが1/1、Enterで先頭へ戻った状態

![文書内検索でBODY-ANCHORが1/1、Enterで先頭へ戻った状態](screenshots/15-document-find-enter.png)

### 本文Redo後にCtrl+S、Saved表示

![本文Redo後にCtrl+S、Saved表示](screenshots/16-body-redo-saved.png)

### タブを閉じ、再度開いて保存済みの編集を確認

![タブを閉じ、再度開いて保存済みの編集を確認](screenshots/17-body-reloaded.png)

### 絵文字・結合アクセントを含む行の切取・貼付後

![絵文字・結合アクセントを含む行の切取・貼付後](screenshots/18-clipboard-unicode-restored.png)

### 選択した日本語行をUnicode文字列で置換

![選択した日本語行をUnicode文字列で置換](screenshots/19-selection-replace-unicode.png)
