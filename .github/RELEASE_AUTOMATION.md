# リリース手順

## 初回設定

GitHub の Actions が版番号更新 PR を作り、その PR の通常CIを自動で起動できるよう、リポジトリの Actions secrets に `RELEASE_AUTOMATION_TOKEN` を登録する。

fine-grained personal access token を作り、対象 repository を `hide212131/hane` のみにし、次の権限を付ける。

- Contents: Read and write
- Issues: Read and write
- Pull requests: Read and write

この token は release branch の push、PR 作成、release ラベルの作成にだけ使う。workflow run の参照や release build の起動には各 job の `GITHUB_TOKEN` を使う。token をログへ出力したり、workflow input に渡したりしない。

`GITHUB_TOKEN` だけでPRを作ると、そのPRを起点とする通常の `pull_request` workflowが承認待ちになるため、通常CIを自動で続ける用途では使わない。

## main のrulesetとworkflow権限

release PR の自動mergeでは `gh pr checks --required` を使う。このコマンドはrequired checkが未設定だと成功扱いにならないため、`main` にrequired status checkを設定する。現在のactive ruleset `main required status checks` では、次の2つを要求している。

- `cargo test / clippy (macos-latest)`
- `cargo test / clippy (windows-latest)`

Release PRではCodeRabbit reviewを省略する。`main` にrequired approval reviewを設定する場合は、release PR用の承認者またはGitHub Actions automation actorのruleset bypassも構成する。承認要件を満たせなければ自動mergeは停止する。

自動merge jobは、job単位の `GITHUB_TOKEN` に次の権限を指定している。

- `contents: write`: tested head SHAを指定したPR merge
- `pull-requests: write`: PR状態の確認とmerge
- `actions: write`: 既存 `Release builds` workflowの起動
- `checks: read` と `statuses: read`: required check/statusの確認

これらの権限は自動merge jobだけに付与し、workflow全体へ広げない。

## リリースの起動

1. Actions の `Request release` workflow を選び、branch `main` で実行する。
2. `version` に `v0.24.0` のような tag 形式を入力する。
3. workflow は main の現在版と最新 tag を確認し、Cargo.toml と Cargo.lock の workspace version だけを更新して release PR を作る。
4. release-only の軽量検査が成功し、GitHubのmergeability計算が確定すると PR をtested head SHAで自動 mergeし、同じ `vX.Y.Z` の既存 `Release builds` を起動する。
5. workflow 完了時に、マージ commit の `scripts/release_catalog.py verify-release` で、公開済みの macOS / Windows の ZIP、`hane-update.json`、`hane-update.json.sig` の署名・版・source SHA・ZIP hash・inventory / bundle metadata が一致することを確認する。

同じ version の tag、release branch、または open PR が見つかった場合は、重複を作らず停止する。版更新以外の変更が見つかった場合は自動 mergeしない。

## 更新対応Releaseの署名設定と初回発行

`Release builds` は、更新用Ed25519鍵・MSIX署名証明書が全てそろわないと失敗し、未署名のReleaseを作らない。Apple Developer ID・Team ID・notarizationの設定は要求しない。必要な名前と用途は [Windows署名手順](../docs/windows-release-signing.md) にまとめている。値は保守者が設定し、リポジトリ・ログ・Issueへ出さない。

- Releaseは Draft として作成し、4つのアセット（2つのZIP、catalog、署名）を再取得して検証した後にだけstable公開する。既存のtag・Release・アセットは上書きしない。
- `catalog_sequence` は公開済みcatalogの最大値 + 1 で、読み取りから公開までを共通のconcurrency group（`hane-release-publish`）で直列化する。公開済みcatalogが無いときだけ、`Release builds` を `workflow_dispatch` で `initial_catalog` を指定して起動し sequence 1 を発行する。`Request release` 経由の自動起動は初回扱いにならず、この場合は失敗する。その時はmerge後のmainから手動で起動し直す。
- 既存tagの再実行を含め、版が既存の最新stableより新しくない場合は公開しない。
- 実署名・二版更新の実機確認（Issue #510 U5）はこの設定だけでは完了しない。

