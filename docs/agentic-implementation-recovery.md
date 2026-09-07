# Claude 実装ワークフローの停止と再開

[全体設計](agentic-development-workflow.md)を補足する運用手順。
対象は `.github/workflows/implement.yml` であり、Local GUI validator の権限や
[ADR-0024](adr/0024-local-gui-validation.md)の実装順序は変更しない。

## Issue #77 で確認した停止

2026-09-07 の [実行 34125927529](https://github.com/hide212131/hane/actions/runs/34125927529)
の `implement` ジョブログでは、次を確認した。

```text
subtype: error_max_turns
num_turns: 81
permission_denials_count: 12
Execution failed: Reached maximum number of turns (80)
```

直接の停止理由は `--max-turns 80` への到達であり、契約の利用上限や
60分のジョブ時間制限、Mac の画面操作の失敗を示すエラーではない。
Claude の進捗コメントでは実装・テスト追加まで済んだとされていたが、
検証と commit / push は未完了だった。終了時に対象ブランチは remote に存在せず、
この実行には取得できる artifact もなかった。進捗コメントは完成コードの証拠ではない。

公開ジョブログは拒否の件数だけを含み、具体的な操作の内訳は含まれていない。
12件の拒否が上限到達の主因だったか、どの権限が不足したかは断定できない。
`Edit` / `Write` が不足したと推測して権限を広げない。

## 再発を減らす方針

処理回数の上限を160回に調整し、60分のジョブ時間制限は維持する。
160回での完了を保証するものではなく、繰り返し不足が起きる場合は作業を分ける。
[公式の設定説明](https://github.com/anthropics/claude-code-action/blob/main/docs/configuration.md#limiting-conversation-turns)
に従い、無制限の実行にはしない。

Claude への指示では、小さな実装がまとまった段階で、長い全体検証の前に
専用ブランチへ commit / push するよう求める。未実施の検証は未実施と明記する。
これは途中保存を促す指示であって、異常終了時に未保存コードを必ず救済する機構ではない。
自動の全ファイル commit や、失敗時の無条件 PR 作成は追加しない。

対象を絞ったテストを先に行い、標準検証も試す。実行環境に依存する失敗を
繰り返し修復したり、拒否された操作を同等の別コマンドで回避したりしない。
不足を記録して、既存の信頼する CI と人の確認へ引き継ぐ。
呼び出し元の権限確認、許可ツール、認証、マージ条件は変更しない。

## 失敗時に残す診断

Claude のステップが失敗した場合だけ、信頼する workflow に埋め込んだ Python 処理で
Action の実行記録を読む。`execution_file` 出力がない場合は、この実行で観測した
`$RUNNER_TEMP/claude-execution-output.json` を確認する。記録がない・形式が違う場合も、
詳細を推測せず `unavailable` / `missing-result` として扱う。

ジョブの Summary に、固定の結果分類、処理回数、所要時間、権限拒否の件数と
ツール種類別の件数だけを残す。未知の文字列は `unknown` / `other` に置き換える。
プロンプト、コマンド引数、ファイルパス、会話本文、エラー全文、認証情報は出力しない。
実行記録の全文を artifact に上げたり、`show_full_output` を有効にしたりしない。

読み取りは runner の一時ディレクトリ直下の通常ファイルに限り、32 MiB を上限とする。
シンボリックリンクやパイプは読まない。Python は `-I` で起動し、作業コピーや
`PYTHONPATH` からモジュールを読み込ませない。診断は原因調査用であり、結果を成功に変えず、
対象コードが環境自体を侵害した場合の証拠改ざんまで防ぐものとはしない。

## 再開手順

まず現在の Issue コメント、実行、remote ブランチ、未完了 PR を確認する。
途中保存されたブランチがある場合は、その内容を確認して再利用する。
同じ実装を重複して依頼しない。

workflow の修正を `main` にマージした後、対象 Issue に **新しいコメントとして**
`/implement` を投稿する。既存の失敗実行の Re-run は、変更後の workflow で開始する方法と
混同しない。GitHub の [再実行の説明](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/re-run-workflows-and-jobs)
では、元イベントと同じ `GITHUB_SHA` / `GITHUB_REF` を使う。

起動したこと、実装を push したこと、PR を作ったこと、CI が通ったことを分けて報告する。
Issue #77 の完了やローカル Mac での実証は、この復旧用変更だけでは完了しない。

## 診断処理のテスト

```sh
python3 -m unittest discover -s .github/tests -p 'test_implement_failure_summary.py' -v
```

workflow に埋め込んだコードそのものを抽出し、正常形式・欠損・破損・不正な値・
機密に見立てた文字列の非出力・大きすぎるファイル・リンクやパイプの拒否を検査する。
このテストは Claude の実行や GUI の実機検証を代替しない。
