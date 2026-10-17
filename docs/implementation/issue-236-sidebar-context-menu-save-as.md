# Issue #236: Work folder sidebar のファイル操作コンテキストメニュー

準備ブランチの基準 main: e82b9a9fe42280bfa78497746bcd7e21c73430c5

この文書は Issue #236 の実装範囲を共有するための準備資料です。この時点では製品コード、テスト、GUI検証を変更・実施していません。

## 目的

Work folder のファイル行とフォルダ行を右クリックし、Markdown 編集中に必要な基本操作を使えるようにします。新しく作ったメモを編集したあと、ファイル行の Save As から Work folder の外を含む任意の場所へ保存できるようにします。

## Current main で再利用する経路

- Save As の native path picker は既にあり、選んだ path を既存の SaveIntent::To 経路で保存します。選択先は現在の Work folder に限定しません。
- 未命名の Work folder メモは既存の復旧 draft へ保存されます。Save As に独自の draft 保存形式や保存状態を追加しません。
- Save As の要求は対象 session と document instance に結び付け、native dialog 中に tab や folder が切り替わっても、別文書へ結果を適用しないようにします。
- atomic save、外部変更検査、autosave、H1 自動命名の現行契約を維持します。既存ファイルの移動・削除を Save As に暗黙で加えません。

## 今回の範囲

ファイル行のメニューに Rename、Move to Trash、Reveal in Finder / Explorer、Copy Path、Save As を追加します。フォルダ行には Rename、Reveal in Finder / Explorer、Copy Path を追加します。

メニューは右クリックされた行を対象にします。Save As は新しいメモを含むファイル行で使え、既存の native Save As dialog から任意の保存先を選べます。成功した場合は既存保存経路に従って session と sidebar の状態を更新します。

キャンセルまたは保存失敗時は、編集中の source、draft、現在の保存先を変えません。既存の保存先を置き換える場合は native dialog の確認を通し、silent overwrite を追加しません。Trash 操作も OS の Trash / Recycle Bin を優先し、dirty session を黙って破棄しません。

filesystem I/O は pointer event の同期経路で実行しません。Rename は既存の inline rename を再利用します。新しい file service、async trait、runtime、保存形式は追加しません。

## 関連する責務と依存

- #308 は H1 自動命名・rename の状態整理、#309 は open/save/draft の要求・結果受理を扱います。
- この Issue は既存 Save As / draft / rename 経路を呼び出す UI 操作を追加します。保存・命名の所有者や契約を変更しないため、#308/#309 の完了は一律の開始条件にしません。実装時に既存契約との衝突が見つかった場合は、その範囲を分離して報告します。
- File-tab context menu からの Save As は別の Issue で扱います。この Issue は sidebar row の menu に限定します。

## 実装・検証の提出条件

- file row と folder row の menu が pointer 位置の対象を処理する。
- Save As が Work folder 外の path を選べる。非アクティブな open session を対象にする場合も、別 session へ保存先や内容を適用しない。
- cancel、dialog failure、write failure、destination conflict、dirty session、Trash、rename/save 競合を、既存の document/session identity と失敗注入で検証する。
- I/O を UI input path で同期実行しないことを確認する。
- workspace tests / clippy と変更に応じた macOS / Windows native GUI scenario を対象 head と結果に結び付ける。未実施の OS・scenario を成功扱いしない。
