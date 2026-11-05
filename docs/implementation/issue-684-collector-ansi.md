# Issue #684: collectorのActionsログANSI制御文字対応

## 目的

Issue #684 の専用fixture PR #690（run `42067625206`、attempt 1）で、初回worker・trusted finalizerは成功し、exact verifierは意図した`aadw_feedback_fixture_expected_value`の失敗を記録した。一方、feedback collectorはverifier job logsの取得で停止した。次のfixture実行に必要な、ログ取得の最小修正を行う。

## 観測と仮説

- collectorの安全な診断は操作名`verifier job logs`、HTTP status不明、`gh exit 1`を記録した。API失敗をHTTP 403等と推測しない。
- GitHubから取得した同じverify job logは348,814文字で、ANSI ESC制御文字を32個含む。失敗したfixture testとcargoのexit 101も同じログで確認できる。
- GitHub CLIの`gh api`は既定でANSI制御シーケンスを含む応答を拒否する実装がある。ログ応答を取得したときの拒否が今回の原因という仮説は観測と整合するが、過去runのstderrが保存されていないため、根本原因の確定ではなく回帰で検証する仮説として扱う。
- API permission不足、302リダイレクト不具合、endpoint不具合と断定しない。

## 変更範囲

- 対象は`.github/scripts/aadw_verification_feedback.py`と、そのCLI／ログ解析に直接対応する`.github/tests/test_aadw_verification_feedback.py`に限定する。
- 完了済みjob logを取得する`gh api`呼び出しで、制御文字を含む応答をバイト列として安全に受け取れる最小の方法を選ぶ。
- raw logはsubprocessの捕捉領域内でのみ扱う。stdout、Actions command stream、workerへの環境変数、認証情報へ流さない。reportへ入れるdiagnostic excerptは既存のANSI除去・秘密値拒否・schema検証を維持する。
- 回帰テストで、ANSI付きの実log相当データから一意なtrusted capture resultを取り出し、返却excerptにANSIが残らないことを確認する。成功logと壊れた/曖昧なcapture markerの既存検査も保つ。
- API endpoint、権限、retry、workerの権限、Claude実行経路、report契約は変更しない。workflow・Policy・Claude手順の意味判断を複製しない。PR #690およびPR #679へ変更を加えない。
- 変更前後のcollector関連テスト、Python compile、workflow wiring構文/検査を実行し結果をPRに記録する。

## 受入確認

- ANSI ESCを含むverify job logをcollectorが処理し、意図的なテスト失敗のreport artifactを生成できる。
- reportは既存schemaとcapture record検証を通り、対象SHA・run/attempt・verify job ID・test/lint結果を保持する。
- workerへ返す診断にはANSI制御文字やcredentialが残らない。
- ANSIのないログ、欠損または曖昧なcapture record、API取得失敗は引き続きfail closedとなる。
