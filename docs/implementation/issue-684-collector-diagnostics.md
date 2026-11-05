# Issue #684 follow-up: collector API失敗の安全な診断

## 観測

- 基準main: `fc768fcfdcf5c59ea3620563a60d8deeb78319bf`。
- PR #688の経路を初めて使ったPR #679 run `42057088136` では、Claude workerとtrusted finalizerは成功し、exact verifierは既存Save As回帰で失敗した。
- 後続collector job `127356144536` も失敗し、Actions logには `GitHub source could not be read` だけが残った。
- `collect` は report生成前にattempt jobs一覧とverify job logsを読む。元runの失敗がどちらのAPIか、HTTP status、permission不足の有無は、このrunからは特定できない。

## 今回の変更

- source API呼び出しに固定の操作名を付け、失敗時はその操作名・HTTP status（stderrから読み取れる場合）・status不明時の `gh` exit codeをActions logへ出す。
- `gh` のstderr、response body、URL、token、その他のcredentialらしき文字列は出力しない。
- attempt jobs一覧とverify job logsの403・404・statusなしを回帰で確認する。statusなしはtransport/client等を含む未特定として扱い、permission不足と推測しない。
- workflow権限、API endpoint、retry動作は変更しない。今回の診断結果だけで権限を追加しない。

## 境界

このfollow-upはcollectorの失敗箇所を識別可能にする変更であり、元runのAPI失敗原因を遡って確定したものではない。実runの検証失敗report返却、同じ実装担当による修正成功、異常系拒否、代表的な実修正の所要時間比較はIssue #684の未達受入条件として残る。PR #679の実装・runには変更を加えず、新しいworker依頼も行わない。
