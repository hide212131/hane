# リリース手順

## 初回設定

GitHub の Actions が版番号更新 PR を作り、その PR の通常CIを自動で起動できるよう、リポジトリの Actions secrets に `RELEASE_AUTOMATION_TOKEN` を登録する。

fine-grained personal access token を作り、対象 repository を `hide212131/hane` のみにし、次の権限を付ける。

- Contents: Read and write
- Issues: Read and write
- Pull requests: Read and write

この token は release branch の push、PR 作成、release ラベルの作成にだけ使う。workflow run の参照や release build の起動には各 job の `GITHUB_TOKEN` を使う。token をログへ出力したり、workflow input に渡したりしない。

`GITHUB_TOKEN` だけでPRを作ると、そのPRを起点とする通常の `pull_request` workflowが承認待ちになるため、通常CIを自動で続ける用途では使わない。

## リリースの起動

1. Actions の `Request release` workflow を選び、branch `main` で実行する。
2. `version` に `v0.24.0` のような tag 形式を入力する。
3. workflow は main の現在版と最新 tag を確認し、Cargo.toml と Cargo.lock の workspace version だけを更新して release PR を作る。
4. release-only の軽量検査が成功すると PR を自動 merge し、既存の `Release builds` を起動する。
5. workflow 完了時に macOS / Windows の ZIP が GitHub Release にそろったことを確認する。

同じ version の tag、release branch、または open PR が見つかった場合は、重複を作らず停止する。版更新以外の変更が見つかった場合は自動 mergeしない。

