# Issue #101 merge 後 GUI 受け入れ検証

この文書は Issue #118 の完了条件に従い、PR #123 を main へ merge した後の trusted GUI procedure で Issue #101 の構文固有シナリオを検証するための記録用エントリである。

検証対象の基点は PR #123 の merge commit `79ebcab77c032367266486c4c1e38abd93fa0170`。この検証 PR はその commit から分岐し、製品コードを変更しない。

確認対象は、PR #123 で `hosted-gui-interaction/5` に追加した太字・斜体・複数行 inline code・quote/list 内構文の境界操作、caret 移動、ドラッグ選択、日本語 IME、Undo/Redo、delimiter の未閉鎖と再閉鎖、保存・再オープン、および各状態の画像・保存 byte・receipt の対応である。

実際の GUI receipt、exact-head SHA、workflow run、最終判定は検証完了後に Issue #118 / #101 へ記録する。過去の PR #123 自身の GUI receipt は変更前の trusted procedure によるため、この受け入れ検証の合格証拠として再利用しない。
