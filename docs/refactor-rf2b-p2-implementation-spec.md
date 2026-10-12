# RF2-B / #302 P2：高さ索引の分割と再開手順

作成日: 2026-10-12 JST  
対象Issue: [#302](https://github.com/hide212131/hane/issues/302)  
全体の設計: [refactor-rf2b-implementation-spec.md](refactor-rf2b-implementation-spec.md)  
状態: 設計の補足。P2の製品実装・テスト・受入は未実施。

## 1. 次に進める作業

**#303の次は、未完の#302に戻ってP2を実装する。** #302の新しいIssueを作ったり、受入済みP1を再実装したりしない。全体の分割方針・P3〜P5・保護契約は既存設計を維持し、本書はP2の具体的な移動対象、テスト、検証環境だけを補う。既存設計第8.1節の「最初はP1」は初回着手の説明であり、今回の再開位置ではない。

#303はP1〜P6がmerge済みで、Issueはcompletedとしてclosed。最後のPR #466のmerge commitは `a69d41f7c5dba827f957d3a6b51aea418bdf66a0`。一方、#302はP1（PR #456）だけが受入済みでopenのまま。#306は#302/#303などの該当領域の分割を前提としているため、同じ表示処理に意味変更を重ねる前に#302の残りを進める。独立した#304等の作業や機能追加を一律に止める方針ではない。

### 調査基準

| 対象 | 2026-10-12に取得した事実 |
|---|---|
| main | `a69d41f7c5dba827f957d3a6b51aea418bdf66a0`。文書ブランチ作成直前にも再確認した。 |
| `crates/presentation/src/lib.rs` | blob `7a940d3962f3503e00df7f9e7992eaf4925e3f99`。`mod source_map`とroot再exportは実装済み。`HeightIndex`等はrootに残る。 |
| P1記録からの差 | `948b9e3f2f0525fa3c7f1176da9a65f6d0f6c380...a69d41f7c5dba827f957d3a6b51aea418bdf66a0`のcompareは9 commits。変更ファイルに`crates/presentation/`はなく、Markdown分割とrelease/CI関連などが加わっている。 |
| CI | `.github/workflows/ci.yml`のblobは `0be582162e76a87128107327db38c211f4932c1e`。release-only判定が加わっている。古いCI手順だけで実行範囲を判断しない。 |
| open PR | 調査時の一覧は#450のみ。取得した変更14ファイルはAI/UIとその設計書で、presentationを含まない。着手前には一覧とheadを再取得する。 |

上記の比較は、現行ソースに既存設計を適用できるかの静的確認である。#303や#302 P1の成功を、これから作るP2候補のテスト結果とは扱わない。

## 2. P2で分ける責務

各ブロックの高さを数値として保持し、先頭からの合計や表示範囲を求める処理を、`height_index.rs`へ移す。高さをどのように決めるかというMarkdown側の意味は、このファイルへ持ち込まない。

次はrootに残す。

- `block_line_span`、`trailing_blank_lines`、`block_heights`、`block_heights_with_disclosure`、`block_height_with_disclosure`。
- `VisualLine`、`VisualBlock`、表示方針、行・ブロックの表示生成、共有解析。
- P1で分離済みの`source_map.rs`、既存`layout.rs`と公開`testing`。

特に#303で分離した`hane_markdown::FenceHeightProjection`と、今回の数値索引`hane_presentation::HeightIndex`は別責務である。前者が計算する非表示行や原文開示の意味を、後者へ統合しない。キャッシュの寿命、画面スクロールの慣性、UIの座標計算、性能最適化も対象外とする。

## 3. 移動一覧と公開範囲

移動元はすべて`crates/presentation/src/lib.rs`、移動先は新しい`crates/presentation/src/height_index.rs`である。直前の説明、属性、derive、フィールド、implを含めて移す。

| 項目 | 移動する内容 | 公開範囲 |
|---|---|---|
| `HEIGHT_CHUNK_TARGET` | 値128と説明。 | privateのまま。 |
| `HeightChunk` | `heights: Vec<f32>`、`total: f32`、`new`、Clone/Debug。 | 型・フィールド・メソッドともprivateのまま。 |
| `HeightIndex` | `chunks`、`sums`、`counts`、`len`とClone/Debug、全impl。 | 型はpub、全フィールドはprivateを維持。 |
| `ScrollAnchor` | `block: BlockId`、`intra_block_y: f32`、`visual_position_hint: Option<VisualOffset>`、Clone/Copy/Debug/PartialEq。 | 型と3フィールドのpubを維持。 |
| `anchored_scroll_y` | 引数、戻り値、全本体。 | pubを維持。 |

`HeightIndex`の公開10メソッドは `new`、`len`、`is_empty`、`update`、`height`、`splice`、`prefix_sum`、`total_height`、`block_at_y`、`visible_range`。privateな6メソッドは `retree`、`tree_prefix`、`search_counts`、`locate`、`locate_insert`、`add_sum`。一部だけrootへ残さない。

`retree`と`add_sum`の既存の`#[expect(clippy::manual_isolate_lowest_one, reason = "unstable on Rust 1.98.1")]`も移す。crate-levelのlint属性はrootに残す。抑制を追加したり、浮動小数点の計算を別方式へ変えたりしない。

### 接続するコード

新ファイルの製品importは次を基準とする。

```rust
use crate::{VisualBlock, VisualOffset};
use hane_markdown::BlockId;
use std::ops::Range;
```

rootへ追加するのは非公開module宣言と、既存3名の明示的な再exportである。

```rust
mod height_index;
pub use height_index::{HeightIndex, ScrollAnchor, anchored_scroll_y};
```

rootで使っている`Range`や`BlockId`は他の処理でも必要なため、移動と同時に一括削除しない。`pub mod height_index`、別名型、呼び出しを中継する新しい関数、`#[path]`による移行接続は作らない。内部可視性を広げる必要はない。`VisualBlock`と`VisualOffset`は既存rootの型を使い、P3で表示型が分かれた後も同じroot公開名から利用できる。

再exportは既存APIを維持する恒久的な入口である。後で撤去する移行aliasではない。Rustのprivate項目と再exportの規則は[Rust Reference](https://doc.rust-lang.org/reference/visibility-and-privacy.html)を参照する。

## 4. 変更しない計算と失敗時の扱い

`new`、`update`、`splice`は、現行の`.max(0.0)`をその位置のまま使う。非有限値を受け付ける新しい仕様や、独自の値補正は追加しない。

`splice`は先頭・末尾の境界chunkと挿入値から置換chunkを作り、範囲外の測定値を保つ。chunkサイズ、配列の結合順、`len`の更新、`retree`の順序を維持する。全ブロックを平坦化する実装へ置き換えない。

`prefix_sum`は指定位置をlenへ収める。`block_at_y`は空なら0、非空なら現行のclampと探索を使う。探索後に最後のchunkへ戻る処理と、0高さの項目をまたぐ条件を変えない。`visible_range`は空なら`0..0`で、非空の終了位置に1を加える現行条件を保つ。

`update`の範囲外panic、`splice`の範囲assertionは、Resultや黙った無視へ変えない。`anchored_scroll_y`はBlockIdに一致する位置を探し、その位置より前の高さに、**`VisualBlock::height()`で上限を取った**内部位置を加える。索引側の`height(index)`へ置き換えない。見つからなければNoneを返す。現在使っていない`visual_position_hint`をこの移動で有効化したり削除したりしない。

## 5. 既存テストは5件だけ移す

次の5件は名前・属性・入力・assertion・本体を維持し、rootの`tests::`から`height_index::tests::`へ移す。新ファイル末尾の`#[cfg(test)] mod tests`に置く。テストmodule内の`use super::*;`は使用できる。

```text
fenwick_updates_and_finds_visible_blocks
the_last_block_stays_visible_when_the_viewport_reaches_the_end
the_last_block_stays_visible_across_a_chunk_boundary
height_splice_preserves_measurements_outside_the_changed_blocks
chunked_height_index_matches_a_flat_model_across_boundaries
```

移す理由は数値索引の更新・端点・chunkを直接検証しているためである。`initial_block_heights_keep_the_caret_owned_fence_row_visible`などのMarkdown由来の高さテストはrootに残す。`stale_non_overlapping_block_rebases`など隣に並ぶ別責務のテストまでまとめて移さない。integration testsとfixtureも元の場所に残す。

## 6. 足りない境界確認を、移動前に補う

上の5件は更新・末尾・chunk境界の検証を持つが、それらだけを根拠に空・0高さ・anchorの全条件を検証済みとは扱えない。外部からの公開入口を確認する`crates/presentation/tests/height_index_contract.rs`を追加する。新しい依存は使わない。同じ条件を満たすテストが着手時点で既にあれば重複追加せず、対応先を記録する。

| 新しいテスト名 | 入力と期待する結果 |
|---|---|
| `empty_height_index_round_trips_through_splice` | 空の索引でlen=0、is_empty=true、height(0)=None、prefix_sum/total_height=0、block_at_y=0、visible_range=`0..0`。`splice(0..0, [5.0, 7.0])`後はlen=2、合計12。`splice(0..2, [])`で元の空状態へ戻る。 |
| `zero_height_entries_preserve_boundary_and_eof_selection` | `[10.0, 0.0, 20.0, 0.0]`で先頭のprefixから順に`[0,10,10,30,30]`、block_at_y(10)=2、block_at_y(30)=3、block_at_y(100)=3。`visible_range(0,30,0)=0..4`。別の`[0.0,0.0]`では末尾検索が1、表示範囲が`1..2`となる現行の扱いを確認する。 |
| `negative_height_inputs_keep_existing_clamping` | `new([-2.0,4.0])`で高さ`[0,4]`。update(1,-3)で`[0,0]`。`splice(1..2, [-1.0,6.0])`で`[0,0,6]`、合計6。いずれも小さい整数値をf32で使い、許容誤差の導入で期待値を緩めない。 |
| `scroll_anchor_keeps_identity_and_visual_height_clamping` | 異なるIDの表示ブロックA/Bを作り、表示上の高さを10/20、索引を`[10,30]`とする。Bへの内部位置3なら13、負値なら10、100なら30。索引のBの高さ30ではなく表示の高さ20で上限を取ることを区別する。存在しないIDと空のblocksはNone。visual_position_hintだけを変えても現行の結果は同じ。 |

最後のfixtureは既存の公開`VisualBlock`フィールドと既存の`VisualLine`生成関数で作る。新しい製品constructorやtest用公開APIを作らない。ブロック数と索引の項目数を通常ケースでは揃え、表示側と索引側の高さだけを意図的に変えて現行の上限計算を検査する。

この表は**現行コードから導いたテスト設計であり、今回実行した成功結果ではない**。移動前のコードで実行し、失敗した場合は期待値・fixture・既存契約を切り分ける。不具合を正しい仕様として固定したり、移動後だけ通るように期待値を変更したりしない。別の製品修正が必要なら分離する。

## 7. 実装の順序と許容ファイル

P2を1つのPRとし、まずテスト追加だけのcommitを作る。変更前の既存suiteと追加テストが通ることを確認してから、別commitで製品コードと既存5テストを機械的に移す。管理用の新Issueや前段の別PRを必須にはしない。契約そのものを変える場合だけ別作業へ分ける。

製品変更は`crates/presentation/src/lib.rs`と新しい`height_index.rs`の2ファイル。追加テストを含める場合は上記integration testの1ファイルを加える。設計・証拠文書は別に保存してよい。`layout.rs`、`testing.rs`、UI、Markdown、Cargo、lockfile、vendor、workflow、バージョンは変更しない。

製品の関数本体をbyte単位で比較し、差をmodule宣言・再export・import・必要な説明リンクに限定する。文字列literalや計算式の空白まで消す一括正規化を、同一性の根拠にしない。既存5テストも本体を比較する。全件数は「変更前既存件数＋実際に追加した件数」で比較し、module prefixが変わるのは指定5件のみとする。

## 8. 変更前後の検証

着手時はcurrent main、#302、本書と全体設計、関連open PR、作業ツリーを取得する。利用者の既存変更を壊さない。repo指定toolchainを使い、その版とOS、base SHA、テストだけのcommit、最終候補headを記録する。P1時点のrustfmt版違いを今回の既知失敗と無条件に引き継がない。

```sh
rustc --version
cargo --version
cargo fmt --version
cargo test --locked -p hane-presentation -- --list
cargo test --locked -p hane-presentation --all-features -- --list
cargo test --locked -p hane-presentation
cargo test --locked -p hane-presentation --all-features
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo doc --locked -p hane-presentation --no-deps
cargo doc --locked -p hane-presentation --all-features --no-deps
cargo fmt --all -- --check
git diff --check
```

対象crate全体の実行には既存integration testsと新規testも含まれるため、同じテストを個別コマンドで毎回二重実行する必要はない。追加テストだけを先に確認する場合は `cargo test --locked -p hane-presentation --test height_index_contract` を使える。

外部公開名は3名を保ち、10公開メソッドのsignature、generic制約、公開フィールド、deriveを前後比較する。通常構成の`pub mod testing`も維持する。import成功やテスト件数の一致だけでAPI全体の同一性を宣言しない。

### 現行CIの注意

現行CIはrelease-onlyの分類後にRustの変更範囲を判定する。製品のP2 PRはRustソースを変えるため、release-onlyや文書だけのskipを受入証拠にしてはいけない。macOS/Windowsの既存workspace all-features tests・Clippy等が実際に走ったことを確認する。通常構成は標準CIのall-features成功だけでは証明できないため、上記のローカルまたは既存の検証経路で別に結果を取る。

runがどのheadに対応するかに加え、実際のcheckoutがPR headか、baseとのmerge結果かを記録する。CIのrun情報にPR headが載っているだけで、head単独をビルドしたと説明しない。baseの更新が検証対象へ影響した場合は取り直す。過去のP1や#303のrunをP2の結果へ付け替えない。

実装はmacOS/Windowsのどちらでもよい。片方しか確認していない状態を両OS成功としない。移動前の失敗は同じtoolchain・条件で切り分け、原因不明の必須check失敗を残したままmergeしない。

## 9. 受入条件・画面確認・戻し方

P2は、移動表・公開範囲・既存5テストの本体が保たれ、追加した境界テストが移動前後で成功し、必要なworkspace/OS検証とcurrent contextのレビューが揃って初めて受け入れる。レビューやmergeの判断は、その時点の[Commander Policy](aadw-command-policy.md)と有効なユーザー指示を参照する。本書へ運用規則を複製しない。

本体・条件・型契約・UI配線・layoutを変えない今回の移動に限り、既存設計第9.4節に従って追加の実画面確認を不要と判断できる。その場合は関数本体の一致と利用側の検証を根拠としてPRへ残す。スクロールが滑らかになった、座標の不具合が直った、速くなったという成果にはしない。画面や意味に変更が必要なら別PRとし、必要な実OS確認を追加する。

証拠は既存の`docs/refactor-evidence/rf2b-issue-302/`を利用し、P2の記録からbase/head、コマンド、終了コード、CI run、レビュー、未実施事項へたどれるようにする。実行していない結果を成功の体裁で保存しない。

戻す場合はP2の変更をrevertする。後続のP3〜P5が依存している場合は必要な後続から逆順に戻し、無関係な機能変更を巻き戻さない。保存形式や利用者データの変換はない。

## 10. P2の次と今回の完了範囲

P2受入後は、既存設計どおりP3（表示型と方針）→P4（行・ブロックの表示生成をまとめて移動）→P5（ブロック組立を子moduleへ分離）の順で進める。P2だけで#302をcloseしない。各段階を受け入れたmainから次のPRを作り、同じ大きなファイルを複数担当で同時に移動しない。P3〜P5の詳細設計は既存文書第5・6・8・9節を使い、毎回最初から設計し直さない。

今回保存したのは、現行GitHub情報とコードを照合した設計補足である。この環境ではCargoとJev CLIの存在を確認できず、製品のコンパイル・テスト、Jev判断、実装worker、GUI、独立レビューは実行していない。設計用PRを保存しただけでmainへ採用済み、P2へ着手済み、または#302完了とは記録しない。

## 根拠

- [#303の完了記録](https://github.com/hide212131/hane/issues/303)、[#302のP1受入記録](https://github.com/hide212131/hane/issues/302)、[#306の依存](https://github.com/hide212131/hane/issues/306)
- [P1記録後から調査mainまでの差分](https://github.com/hide212131/hane/compare/948b9e3f2f0525fa3c7f1176da9a65f6d0f6c380...a69d41f7c5dba827f957d3a6b51aea418bdf66a0)
- [調査mainのpresentation実装・テスト](https://github.com/hide212131/hane/blob/a69d41f7c5dba827f957d3a6b51aea418bdf66a0/crates/presentation/src/lib.rs)
- [調査mainの全体設計](https://github.com/hide212131/hane/blob/a69d41f7c5dba827f957d3a6b51aea418bdf66a0/docs/refactor-rf2b-implementation-spec.md)、[現行CI](https://github.com/hide212131/hane/blob/a69d41f7c5dba827f957d3a6b51aea418bdf66a0/.github/workflows/ci.yml)
