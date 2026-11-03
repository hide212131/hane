# Issue #591: クリップボード画像貼り付けの詳細設計

## 状態・正本

対象は [Issue #591](https://github.com/hide212131/hane/issues/591)。2026-11-04の調査基準はmain `1e0d345e46d8c1805659968f8fad1148b0ae37a8`。本書は実装用Draft PRの準備文書であり、製品コード・テスト・実OS検証は未実施。要求・範囲・受入条件はIssue、具体的な実装仕様は本書を正本とする。

利用者の原文は「クリップボード画像をペーストできる機能の実現。Issueにあったはず。imagesフォルダを無ければ勝手に作ってファイルを置くのが自然かな。」。追加質問への回答で「文書の隣の images/」が指定された。これにより既存Issueの保存先初期値 `assets/` を `images/` に更新する。過去に手書きしたassets参照を移動・変換しない。

今回のPRは実画像1枚の通常貼り付けを先行実装する。PNG/JPEGファイルのドロップはIssue全体に残る後続範囲で、このPRの受入・mergeでIssueをcloseしない。新しい添付形式や全製品に及ぶ保存契約は導入せず、既存設計に従う局所拡張なので新ADRは作らない。

## 1. current mainの事実と変更範囲

- `crates/ui/src/actions.rs` のPasteは `read_from_clipboard().and_then(|item| item.text())` で文字列だけを挿入する。本文以外のfocus guardと `input_target()` を維持する。
- `crates/session/src/session.rs::resource_resolver()` は `self.file.directory()` を基準にする。`resource.rs` の相対パスは文書ディレクトリ基準であり、プロセスcwdや常に作業フォルダrootを使う契約ではない。
- 保存・draft接続は `crates/ui/src/view/session_save.rs`。下書きには既存 `target_directory`、`DraftId`、`DocumentInstance` とsnapshot順序があり、新しい下書き管理を増やさない。
- 範囲置換は `view/editor_context_menu.rs` の既存guardと `Editor::replace_range_recorded` を利用する。typingに結合し得る通常Insertで画像記法をばらばらに挿入しない。
- 現行platformは `vendor/gpui-pre-macos/src/pasteboard.rs` と `vendor/gpui-pre-windows/src/clipboard.rs`。旧 `vendor/gpui` を変更・検証対象にしない。core全体を新たにvendor化しない。
- macOSのPasteboard::readはfile paths、plain、imageの順で早期returnするため、混在画像の取得を本文だけの画像優先契約へ対応させる必要がある。Windowsは一回のClipboardGuardで形式を列挙し、PNG/JFIF等とCF_DIBを認識する。存在するコードだけで実スクリーンショット受入済みとしない。
- #596の採用済み [clipboard詳細設計](issue-596-rich-clipboard.md) と同じsnapshot・dispatcherへ合流する。#596は設計保存済み、製品実装は調査時点で未開始。先に本件で入口ができた場合は#596がそこへ接続する。HTML変換自体を本PRに追加しない。

## 2. 保存先と画像データ

保存済み文書 `/notes/sub/note.md` なら `/notes/sub/images/` を自動作成し `![画像](images/<自動名>.png)` を参照する。作業フォルダrootへ固定しない。workspace内の無名文書は既存の保存予定 `target_directory` を使い、名前入力・先行保存を要求しない。復旧したdraftも同じbaseで解決する。現行resolverが無名文書のbaseを持たない箇所では、既存draft targetを文書の有効baseとして一箇所で接続する。cwdを代用しない。

保存予定directoryのない単独無題だけは既存の初回保存UIで保存先を確定する。キャンセル・失敗なら画像を作らず本文・選択を保つ。保存先確認は必要な通常操作であり、workspace下書きにも一律に要求しない。

初回のclipboard入力はOSの実ラスタ画像1枚。出力はPNGに統一し、透明度と向きを保つ。PNG/JPEG/TIFFやDIBなどOSが実画像として提供する表現を必要な範囲でdecodeし、SVGだけ、ファイルパスだけ、HTMLのimgやURLだけを実画像と推測しない。ネットワーク取得をしない。同一画像の複数形式は1枚として扱い、複数独立項目は合成せず対応範囲を知らせる。OSが複数項目を区別できる範囲を明記する。

ファイル名はASCIIの生成名と `.png` を使う。乱数等の生成だけに依存せず排他的新規作成で衝突時に再生成し、既存ファイルは上書きしない。decode/encodeを完了してから保存し、不完全なファイルを完成参照として公開しない。imagesが通常ファイル、権限拒否、ディスク不足、読み取り失敗は本文不変で理由を通知する。作成途中のcleanupはこのjobが作りまだ参照を公開していないファイルだけ。既存画像やundo後の画像を削除しない。

入力bytes、画像寸法、decoded画素数、出力bytesには有限の上限を設ける。具体値は実装担当が採用するdecoderと既存制限を確認して定数と境界テストに固定し、PRへ記録する。巨大画像で無制限確保をせずUI入力を塞がない。

## 3. 貼り付けと履歴

最初に既存のfocus・入力先・閲覧モード・IME guardを確認する。本文以外は既存text経路、編集禁止本文ではファイル作成も開始しない。未確定IMEを暗黙に消さず、既存の確定手順で安全に整理できなければ中止する。

本文の通常Pasteでは同じclipboard snapshotの実画像を優先し、実画像がなければ従来のtext貼り付けを保つ。#596のplain貼り付けが既に導入された場合は画像を保存せずplainを選ぶ。画像保存やdecodeに失敗した後、黙って別表現のtextで選択本文を上書きしない。

画像優先のためにOS clipboardを別々に読み直さない。macOSは同一pasteboard世代を前後のchangeCountで確認し、Windowsは同一ClipboardGuard内で取得する。#596のrich snapshotと公開拡張契約に沿い、画像・plainと将来のHTMLを同一のtransportへまとめる。通常GPUIの全入力欄の優先順位を画像へ変更しない。Busy/ChangedDuringRead/画像不正を空clipboardと区別する。

通常の本文位置ではselectionを置換し、選択がなければcaret位置へ挿入する。既存表示が対応する単独行の画像記法となるよう、必要な前後改行を含む一回のrecorded replacementとする。不要な空行を積み重ねない。リスト・引用・表・コード等の安全性が保証できない位置は保存開始前に拒否し、通常段落への挿入を案内する。周囲の構文・本文を削除して表示を成立させない。

画像保存成功と対象鮮度を確認してから参照を挿入する。1回のUndoで選択元本文と改行まで復元し、Redoで同じ画像を参照する。前後のtypingと履歴を分離し、Undo/Redoは画像ファイルを削除・再生成しない。本文保存だけが後から失敗した場合はdirty本文と画像を保ち、通常保存を再試行できるようにする。

## 4. 非同期・文書の寿命

UI threadで大きな画像変換・I/Oや文書全体の再parseをしない。要求時にSessionId、DocumentInstance（workspace epochを含む）、revision、selection、保存base、draft identity、job世代を捕捉する。完了直前に対象文書が存在し同じinstance・revision・selection・baseで編集可能であることを確認する。最初は複雑な位置rebaseを追加せず、不一致なら中止を採用する。タブ切替で別文書へ入れない。元タブが非activeなら中止してよい。

close/adopt/reload、workspace切替、本文・selection変更、rename/Save Asなどで有効baseが変わったjobを古い前提で確定しない。同じSessionIdの再利用だけで受理しない。連続貼り付けの順序も明示し、後のjobが先行jobでrevision不一致になった場合は安全中止を通知する。状態表示の解除も同じjob世代に限定する。

同一directoryのH1命名・renameとdraft復旧は画像baseを維持する。生成画像を持つ文書の別directoryへのSave Asでは参照中の生成images画像を新しいimagesへコピーし、衝突時は参照を更新する。画像コピー完了後に新しい本文保存を確定する。コピー失敗で成功通知せず、元の本文・画像・未保存状態を保つ。元画像は削除しない。任意の外部画像・手書き相対参照を包括管理しない。生成参照の識別は命名規則と管理する範囲を狭く定義し、永続manifestやMarkdown独自属性を新設せず、再読込後も識別が必要なことをテストする。

## 5. 依存と実装順

#308/#309の命名・保存・draft責務には上記identity/base契約を守って接続する。Issue全体やRF0/#297の完了は待たない。調査時に同領域の実装PRはなく、既存session/FileService/DraftStoreを使えるため一律の先行merge依存を置かない。共有保存状態の変更が必要になれば必要最小限の整理だけ別PRに分け、本機能と混ぜない。

進行中PR #675（未保存タブ破棄）と#678（削除後タブを閉じる）はclose/instance失効で接点がある。#674/#676はリンク入力・解析に関係するが画像表示の新旧二重実装を理由にしない。実装開始時のcurrent head/main/diffで責務の重なりを再確認する。#18全体の画像表示拡張や#596全機能を先行条件にしない。

製品実装はClaude担当。純粋なPNG保存・衝突処理と参照生成、platform snapshot、UI job/recorded replace、draft/base/Save Asの順で既存境界へ接続する。必要ならUIの新規 `clipboard.rs` / `view/clipboard.rs` に分離し、名前自体は強制しない。vendor変更時は現行platformのHANE-PATCH.mdも更新する。

## 6. 受入と検証

| 条件 | 自動検証 | macOS / Windows実OS |
| --- | --- | --- |
| 実画像1枚→PNG→表示 | PNG透過、形式decode、画像とplain混在、同一世代、壊れた画像・サイズ上限 | OSのスクリーンショットと画像アプリから実clipboard貼付、色・寸法・透過確認、保存再読込 |
| 文書隣接images | ネストしたdocument、images未存在/既存file、生成名衝突・同時保存、Unicode/空白/括弧path、区切り `/` | 文書+imagesを一緒に移動して開く |
| 未保存/draft/Save As | target_directory、draft復旧、同directory命名、別directoryコピー・衝突・失敗で元本文保持 | workspace新規へ無名貼付→再起動復旧→命名、単独無題の保存/キャンセル、別場所保存 |
| 編集・失敗・寿命 | 1Undo/Redoとtyping分離、選択置換、IME/閲覧/focus拒否、遅延中のrevision/selection/tab/close/reload/workspace/base変化で誤挿入なし | 両OSの実日本語変換、検索/設定/renameの通常paste、連続貼付、閉じる/切替 |
| 保存失敗 | permission/partial write/ディスク不足の注入、既存ファイル不変、参照挿入なし、本文保存失敗時dirty維持 | 実OSの書込不可先でエラー表示と本文不変 |
| 応答性・既存表示再利用 | 必要な範囲だけ更新・可視画像ロード、上限境界 | 同じRelease/fixtureで変更前後の大文書入力・scroll・RSS/処理時間を記録 |

主な検証はfocused Rust/GPUIイベントtests、workspace all-features tests/clippy、通常feature/release build。vendor testsはworkspace外なので変更したplatformのmanifestから別途実行し、成功結果と対象SHAを記録する。テスト名や未実行コマンドを成功証拠にしない。

GUIは `.agents/skills/hane-gui-test/SKILL.md`、ADR-0024、`docs/gui-test-recovery.md` に従う。起動時にPID・実行path・隔離state・対象head/baseを記録する。失敗時は層を切り分け安全に再起動・再接続し、軽量操作確認後に元scenarioを再実行する。Windowsは許可済みWindowsチャットからWindows上のChatGPTアプリで直接操作し、Mac経由VM操作は使わない。必要evidence未取得はunknownのまま残し受入しない。本準備時点ではGUIを起動しない。

## 7. 引き継ぎ時点の未確認

製品実装、有限サイズ上限の具体値、生成画像の再読込後の識別、native mixed-format fixture、Save Asの失敗注入、変更前性能、両OS GUI、current-head CI/reviewは未実施。利用者指定済みの保存先と、これら実装・検証上の残項目を区別する。

Jevは`typesafe / jev-latest`要求に対し`jev-1.14.0`で、既存#591の再利用とpaste先行契約準備を選択（confidence 0.98、801 input/43 output tokens）、続いて本書の保存・履歴・寿命・snapshotの設計案文書化を選択（0.96、876/35 tokens）。取得応答に基づく記録であり製品受入判断ではない。Claude依頼・受理確認以降は別PR担当チャットへ委ねる。
