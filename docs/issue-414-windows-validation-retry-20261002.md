# Hane Issue #414 — Windows実画面検証 再試行記録

## 判定

この再試行では、候補ソースSHA `77b89576fa906f4e562e069906fea245966e8cd0` に対するWIN-01〜WIN-06の画面操作を実行できなかった。画面操作helperが起動後のウィンドウ列挙でタイムアウトし、規定のsession reset後の再試行も失敗したため、全項目を **BLOCKED（未実施）** とする。前回記録の結果を今回のSHAへ転用していない。

| シナリオ | 判定 | 今回の実画面操作 |
|---|---|---|
| WIN-01 | **BLOCKED** | 未実施。現行候補のHaneウィンドウを取得できなかった。 |
| WIN-02 | **BLOCKED** | 未実施。現行候補のHaneウィンドウを取得できなかった。 |
| WIN-03 | **BLOCKED** | 未実施。現行候補のHaneウィンドウを取得できなかった。 |
| WIN-04 | **BLOCKED** | 未実施。現行候補のHaneウィンドウを取得できなかった。 |
| WIN-05 | **BLOCKED** | 未実施。現行候補のHaneウィンドウを取得できなかった。 |
| WIN-06 | **BLOCKED** | 未実施。現行候補のHaneウィンドウを取得できなかった。 |

## 対象とビルド

- リポジトリ: `hide212131/hane`
- branch: `codex/issue-414-p1-search-core`
- 実装候補SHA: `77b89576fa906f4e562e069906fea245966e8cd0`
- base同期: `main` SHA `46154b1eff5487ea802a6def5e596b609711edc6` をmerge commit `181e5cedbf2f57ed7a996abce16d6412785d087b` で取り込み済み。
- Windows: Windows 10 Home、version 2009、build 26200、ARM64
- Work folder: `win-ui-test-414-retry-20261002`。指定どおりAlpha.md、Case.md、Other.mdの3ファイルを作成し、Alpha.md本文を読み戻して確認した。フォルダ選択後の画面確認は未実施。
- ビルド: `CARGO_PROFILE_RELEASE_DEBUG=0`、`CARGO_PROFILE_BUILD_OVERRIDE_DEBUG=0`、`CARGO_INCREMENTAL=0` を指定し、`cargo build --release -p hane --locked -j 1` を実行。成功。
- バイナリ: `target/release/hane.exe`、SHA-256 `386641FA035A4ACA405EF5A08A79E77E39870C4239DFB81BCF4316482EB81BF2`。このバイナリは上記実装候補SHAのソースからビルドした。
- 以前起動していたHaneは画面上で閉じ、`list_apps` が対象Haneなしを返したことを確認した。

## 画面操作helperの状態

base同期直後のpre-fix SHA `181e5cedbf2f57ed7a996abce16d6412785d087b` のバイナリを起動するため `sky.launch_app` を呼んだが、15秒後に `computer-use request timed out: launch_app` となった。続く `sky.list_apps` もタイムアウトしたため、指示されたとおりJavaScript sessionをresetして再初期化し、同じ軽量確認を一度再試行したが、再度 `computer-use request timed out: list_apps` となった。その後のEsc修正を含む最終候補 `77b89576fa906f4e562e069906fea245966e8cd0` のバイナリは起動できていない。これ以上のWindows UI操作は行っていない。PowerShell等を使ったUI操作への置換もしていない。

この再試行ではテスト用Work folder、本文、IME候補、検索結果の画面キャプチャは取得できていない。以前のSHAに属する画像を今回の証拠として流用していない。

## 自動テストと起動確認の区別

- `cargo fmt --check`: pass。
- `cargo test -p hane-ui --lib content_search --locked -j 1`（debug情報無効）: **16 passed、0 failed**。Escで検索結果から本文へ移動した後も検索を終了でき、本文IME変換中は検索モードを維持する回帰テストを含む。
- `cargo build --release -p hane --locked -j 1`（上記メモリ設定）: pass。これはビルド確認であり、起動確認・画面操作成功を意味しない。
- `CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0` を指定した `cargo clippy -p hane-ui --all-targets --locked -j 1 -- -D warnings`: Windows `windows 0.62.2` crateのコンパイル中にメモリ確保失敗（`STATUS_STACK_BUFFER_OVERRUN`）。lint自体は完了していない。
- WIN-01〜WIN-06: 画面操作なし、すべてBLOCKED。自動テストやビルド成功を実画面PASSとして扱っていない。
