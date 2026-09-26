# CodeRabbit Automatic Review validation

Tracking: #373

この文書は、AADW v3 の CodeRabbit Automatic Review / Automatic Incremental Review が実際に動作することを確認するための一時的な docs-only 検証用です。

## Baseline

- この初回commitでは、PR作成後に `@coderabbitai review` / `@coderabbitai full review` を投稿しない。
- CodeRabbitが自動で初回レビューを開始・完了することをGitHub上の実結果で確認する。
- GUI validationは不要。

## Incremental probe

- 初回Automatic Review完了後に、この節だけを2commit目として追加する。
- レビュー要求コメントは投稿しない。
- CodeRabbitが前回headからこの新headまでの追加差分をAutomatic Incremental Reviewとして自動レビューすることを確認する。
