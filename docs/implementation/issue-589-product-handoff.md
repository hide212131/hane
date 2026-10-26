# Issue #589 製品実装の引き継ぎ

対象は [Issue #589](https://github.com/hide212131/hane/issues/589) の文書内置換全体。実装開始基準は main `4f11a6675381838a33653ba0fb0ad62b364f34c9`。

採用済み設計は [issue-589-document-replace.md](issue-589-document-replace.md)。設計PR #621 は merge済みであり、この実装PRとは区別する。要求・受入条件はIssue本文を正とする。

現在文書の1件/全件/空文字への置換、文書全体/明示的に取り込んだ選択範囲、対象と実変更件数の表示、原子的な適用と一回のUndo/Redo、古い非同期結果の拒否、IME/フォーカス/本文履歴の分離、既存保存通知への接続を実装する。設計§8の回帰テスト・両OS native GUI・大文書測定が必要。

正規表現、複数行検索・置換、複数ファイル、差分preview、保存形式変更、全域リファクタリング、AADW保護パスの変更は対象外。open PR #620 は設定画面の機械的分離であり、本件に混ぜない。

この準備commit時点では製品コード・テストは未実装、build/test/lint・性能・macOS/Windows native GUIは未検証。Claude workerが実装し、trusted finalizerとexact-head verify、required CI、current-head review、必要な実機受入を別々に観測する。未実施・unknownをpassへ読み替えず、全受入条件を満たすまでIssue #589を閉じない。
