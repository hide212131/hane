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

## 実行中の進捗を Actions のログに表示する

### 採用する方法と理由

2026-09-07 の要望により、終了時の診断だけでなく、Claude が何をしたかを実行中にも
表示する。`Implement issue with Claude` の `implement` ジョブが concurrency の枠を
取得して実行を開始した後、`repository_dispatch` で `Claude progress log` の独立した
read-only 実行を起動する。既に公開されている Claude の Issue コメントから進捗の文章と
チェックリストの差分を表示し、見る場所はこの実行の `Follow Claude's public progress`
ステップ出力である。実装ジョブの queue 待ち中には dispatch されないため、監視が Claude の
開始を遅らせない。監視実行は実装ジョブとは別だが、同じ Actions workflow の実行履歴で確認できる。

[使用した Action の出力処理](https://github.com/anthropics/claude-code-action/blob/9c5ddab2e6d17b83ea679153b31f1d5f023cf636/base-action/src/run-claude-sdk.ts)
は、全文表示をしない場合、初期化と最終結果以外をログから省く。
`show_full_output: true` は作業の文章だけでなくツール出力なども含むため使わない。
[公式の安全上の注意](https://github.com/anthropics/claude-code-action/blob/9c5ddab2e6d17b83ea679153b31f1d5f023cf636/docs/security.md)
も参照する。非公開の実行記録を公開用に丸ごと読み出す処理や、上流 Action の改変は追加しない。

Claude には、開始、設計を読んだ後、実装後、検証の前後、push 後に、既存の公開コメントを
短い日本語で更新するよう指示する。「実装しました」と「テストで確認しました」を分ける。
更新には run ID と attempt の識別行を残す。これは進捗の報告を促す指示であって、
一定時間内の報告や作業完了を保証するものではない。

### 表示する内容

約15秒ごとにコメントと実装ジョブを確認し、新しい行を時刻付きで出力する。
同じ行を毎回並べず、チェックを戻した場合などの修正も表示する。
報告がない間は約60秒ごとに、表示処理の経過時間と、新しい報告がない時間を表示する。
定期表示は **監視処理が動いていることだけ**を示し、Claude 自身の動作や進捗の証拠にしない。

表示例であり、実際の検証結果ではない:

```text
[13:12:00 UTC] [Claude の報告] 設計文書を確認しました。次に実装します。
[13:15:00 UTC] [Claude の報告] 最小コマンドを追加しました。これからテストします。
[13:16:00 UTC] [監視] 表示処理は継続中（経過 240 秒）。新しい進捗報告は 60 秒間ありません。Claude の動作継続を保証する表示ではありません。
```

実装ジョブが終了したら、GitHub が返した `success` / `failure` 等を表示して監視を終える。
この `success` は実装ジョブの終了結果であり、後続 CI や GUI 検証の成功ではない。
コメント内の完了報告をコードの保存・テスト成功・マージ可否の証拠には使わない。

### 読み取り範囲と制約

監視処理は、既存の `authorize` を通過して concurrency の枠を取得した `implement` ジョブが
dispatch した場合だけ起動する。独立した GitHub-hosted runner で、`repository_dispatch` 実行の
default branch `github.sha` にある監視スクリプトを読み、対象ブランチのコードや設定は実行しない。
権限は `contents: read` / `actions: read` / `issues: read` のみで、checkout に認証を残さない。
監視スクリプトの API 操作はコメントとジョブの読み取りだけで、コメント・status の書き込みや
再実行は行わない。Claude 用の OAuth 認証情報は渡さない。

コメントは対象 Issue、公式の Claude bot / App の ID、実装ジョブの開始・終了時刻で絞る。
run ID と attempt の識別行、または正確な実行リンクを優先する。
上流のコメント更新ではリンクが消える場合があるため、同じ Issue の直列化された実装ジョブ中に
作られた公式コメントが一つだけなら、進捗表示用の代替対応付けを許す。
複数あって対応を特定できない場合は推測せず、報告が未確認である旨を定期表示する。
一度選んだコメント ID は途中で別のコメントに切り替えず、前回 attempt のコメントも再利用しない。
この代替対応付けを、厳密な検証結果の受領処理へ流用しない。

出力元は既に公開されたコメントに限る。コードブロック・HTML コメント等は省き、
既知の認証情報の形式を伏せ、端末制御文字と Actions の命令として解釈され得る文字列を無効化する。
未知の機密を完全に検出できるわけではなく、そもそも公開コメントに秘密情報を書かせない。
生の API エラー・レスポンス本文・SDK の会話履歴は出力しない。

1応答は8 MiB、ページ取得は5ページ、1コメントは100行・1行400文字、実行全体は4,000行を上限とする。
API 取得が5回続けて失敗したら理由を固定文で表示して監視だけ終了する。
監視は63分を上限とし、監視実行自体にも65分の制限を置く。
監視実行は `continue-on-error: true` とし、終了・取得失敗は実装の成否を変更しない。
既存の CI dispatch は引き続き `implement` だけに依存する。
監視を成功扱いにして実装の失敗を消したり、表示の失敗を根拠に実装を再依頼したりしない。
監視は実装開始後にだけ追加の hosted runner を1つ使う。実装ジョブの queue 待ち中に
runner を確保せず、実装ジョブが Claude を起動する前に dispatch する。監視実行は
実装ジョブの終了を確認してから自動終了する。起動通知に失敗しても、実装ジョブは継続する。

今回の対象は `/implement` の経路だけであり、別の Claude 自動修正 workflow への適用は含めない。
マージ前・開始済みの古い実行には後から表示を追加できない。マージ後の新しい `/implement` で確認する。
次の実行では、完了前の進捗表示、報告がない間の定期表示、終了結果をそれぞれ確認する。
現在の確認は自動テストによる模擬応答までで、Actions 上の実動確認は未実施である。

監視処理の自動テスト:

```sh
python3 -m unittest discover -s .github/tests -p 'test_claude_progress.py' -v
```

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

## Issue #83: 停止・未完了理由を対象 PR / Issue へ自動コメントする

Issue #80 / PR #81 では、`.github/workflows/**` への書き込み権限不足で
必要な workflow ファイルだけ適用できず、停止理由が Issue 側の Claude コメントに
埋もれて PR からは分からなかった。これを受け、`/implement`（`implement.yml`）と
自動修正 worker（`claude-fix.yml`）の両方に、Claude の実行が正常に完了しなかった
場合の安全な診断コメントを追加した。

対象とする状態: Claude ステップ自体の failure / cancelled / skipped、および
success でも `.github/workflows/**` への書き込みが権限不足で拒否された場合
（`permission_denials` に該当パスが残る）。

診断ロジックは「失敗時に残す診断」節と同じ理由で、`.github/scripts/*.py` のような
チェックアウト後に読み書きされ得る別ファイルにはしない。Claude 実行後の working tree
には agent が書いた内容が残り得るため、ディスク上のスクリプトを信頼しない。
代わりに、既存の `Summarize Claude failure safely` と同じ形で、各 workflow の
`run:` ブロックへ Python ヒアドキュメントとして直接埋め込む。埋め込みコードが
公開するのは固定文言・既知の enum 分類・許可した文字種のパス名だけで、
プロンプト・コマンド引数・会話本文・認証情報は出力しない。

対象 PR がまだない `/implement` 初期段階（Claude ステップの failure / cancelled）は
Issue にコメントする。workflow ファイルの書き込み拒否は PR 作成後にしか分からないため
PR にコメントする。`claude-fix.yml` 側は常に PR 上で完結する。

重複防止と自動回復の表示は、コメント本文末尾の HTML コメント
`<!-- hane-stall: number=<Issue/PR番号> stage=<段階> sha=<SHA or none> --> `
を鍵として、同じ鍵の既存コメントを検索し、なければ新規作成、あればその場で
本文を上書き（PATCH）する方式にした。個別の通知サービスは作らず、既存の
`gh api` 呼び出しパターンをそのまま踏襲する。`claude-fix.yml` の自動修正が
その後成功した場合は、同じ対象SHAの鍵のコメントを「解決済み」表示に上書きする。

自動テスト:

```sh
python3 -m unittest discover -s .github/tests -p 'test_claude_stall_reports.py' -v
```

`implement.yml` に埋め込んだ2つのブロック（Issue 向けの stall report、
PR 向けの workflow-denial report）を抽出して実行し、理由分類・redaction・
パスの許可文字種チェック・重複排除・上限件数を検査する。`claude-fix.yml` 側は `test_claude_fix_notifications.py` が実際の通知ステップを抽出し、GitHub APIの代替を使って投稿・重複更新・解決済み表示・中止・偽造markerの拒否を検査する。本番GitHubへのコメント配送はこのローカルテストの対象外。
