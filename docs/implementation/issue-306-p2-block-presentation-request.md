# Issue #306 P2: ブロック表示の入力を具体的なrequestへまとめる

## 目的と開始時点

対象は [Issue #306](https://github.com/hide212131/hane/issues/306) の変更手順3の一部である。`crates/ui/src/line.rs` の9引数の入口と、それへ接続するテスト専用wrapperを、一つの具体的な入力型と入口へ移行する。表示・解析の意味や処理量を変えない。

- 調査・着手基準: main `29f7943ce225016ebf3f76c1115857d887388103`。
- 前段P1はPR #540で受入済み。`EditorView::current_joined_parse` と4利用者の参照条件を維持し、再実装しない。
- 要求の正本はIssue #306、全体計画は `docs/refactor-plan.md` RF4 と `docs/refactor-execution-plan.md`、前段の契約は `docs/implementation/issue-306-p1-joined-parse-lookup.md`。
- この文書の初回commitはClaude実装用PRの準備であり、製品コード・テストは未変更、P2のbuild/test/lint/GUI/性能測定は未実施である。現在の実装・検証状態はPRの記録を参照する。

既存ADR-0005/0018/0025/0029の判断を変えず、汎用contextや新しいADRは追加しない。#301全体の完了を主張せず、受入済みの背景解析分離と#302/#303の境界を使う。

## 実装する変更

### 入力型と一つの入口

`line.rs` に `pub(crate) struct BlockPresentationRequest<'a>` を置く。fieldは次の9個だけとし、すべて `pub(crate)`、参照は共通の借用期間 `'a` とする。

| field | 型 | 既存引数からの対応 |
| --- | --- | --- |
| `editor` | `&'a Editor` | 現在の編集状態への同じ参照 |
| `block` | `&'a IndexedBlock` | 同じ対象blockへの参照 |
| `visible` | `&'a Range<usize>` | 同じ物理行範囲への参照 |
| `joined` | `Option<&'a JoinedParse>` | 呼出元が選んだ同じ保存済み解析結果 |
| `list_projection` | `Option<&'a ListProjection>` | 同じ表示用リスト情報 |
| `fence_height_projection` | `Option<&'a FenceHeightProjection>` | 同じコード区切り行の高さ情報 |
| `table_projection` | `Option<&'a TableProjection>` | 同じ表情報 |
| `forced_disclosure` | `Option<SourceRange>` | 同じ検索一致範囲、または従来どおりNone |
| `line_height` | `f32` | 同じ倍率反映済み行高 |

新しい通常入口は `pub(crate) fn presented_block(request: BlockPresentationRequest<'_>) -> Option<VisualBlock>` とする。現在の同名の4引数・`#[cfg(test)]` wrapperは削除し、新入口は通常ビルドでも利用する。

現在の9引数関数 `presented_block_with_table_projection` の本体を新入口へそのまま移す。先頭でrequestを分解して現在と同じローカル名を束縛し、その後の `let document = editor.document();` 以降の処理順、式、分岐、返却値を保つ。9引数関数は撤去し、互換wrapperを新設しない。不要になった当該関数の `clippy::too_many_arguments` 抑制も除去する。他のlint抑制は触れない。

この型は一回の関数呼出しの入力を借用するだけである。cache・永続state・Arc・Box・trait object・builder・Default・暗黙のprojection補完・全体clone・所有権移動・新しいrevision判定を追加しない。公開crate APIへ再exportせず、`hane_presentation::present_block_with_table_projection` は変更しない。

### 呼出元と旧wrapperの移行

すべての呼出元でfield名を明示したrequestを作り、新入口へ渡す。引数の式と評価順を保ち、元の引数順にfieldを並べる。構築だけのために新しい解析、追加のindex lookup、状態更新を行わない。

| 旧入口 | 移行時に保持する値 |
| --- | --- |
| `presented_block(editor, block, visible, joined)` | list/fence/table/forcedはNone、line_heightは従来のDEFAULT_LINE_HEIGHT |
| `presented_block_with_list_projection(...)` | listとline_heightは既存値、fence/table/forcedはNone |
| `presented_block_with_projections(...)` | list/fence/line_heightは既存値、table/forcedはNone |
| `presented_block_with_table_projection(...)` | 9引数を全fieldへそのまま対応させる |

上のテスト専用wrapper3個も、全利用者の移行後に同じPR内で撤去する。旧入口と新入口を併存させず、新しいテスト用互換wrapperも作らない。既存テストのfixture、期待値、テスト名・属性、assertionは変えず、呼出しと必要なimportだけを移行する。

`view.rs` の通常利用者は `layout_around`、`neighbor_row_target`、`cached_block`。前二者のforced_disclosureをNoneとする判断、後者の検索一致範囲、各projectionの取得元、line_height、visible範囲を保つ。`current_joined_parse` の呼出しとその結果を変えない。`disclosures_are_current` と `expected_block_disclosures` の入口・処理は今回変更しない。

調査で参照を確認したファイルは `line.rs`、`view.rs`、`shape.rs`、`view/basic_editing_keys.rs`。実装担当は変更前後に作業tree全体をGrepし、検索索引に出ない大きなファイルやテスト内参照も含めて漏れを確認する。上記以外に実在する呼出元が見つかった場合は、その呼出しとimportの機械的移行だけを行い、追加pathを結果に報告する。別責務へ変更を広げない。

## 必ず維持する契約と対象外

- P1のJoinedParse参照はBlockId、現在の文書revision、外側のblock全体source rangeの一致を保つ。内部parseのrangeへ比較対象を替えず、非交差編集でもrebaseしない。新requestの構築を有効性の証明とみなさない。
- BlockIndexStateの条件付きrebase、正式/暫定結果の優先順位、表示用ListProjectionと同期編集用ListEditProjectionの区別を維持する。
- `block_context`、`block_lines`、`clipped_fence_lines`、`range_disclosure`、`expected_block_disclosures`、下位presentation関数の処理を変更しない。画面外delimiter、末尾空行、表区切り行、コード区切り行、選択・IME・検索による記号表示の条件を保つ。
- JOIN_SYNC_LINE_BUDGET、JOIN_SYNC_BYTE_BUDGET、同期全文parseの条件、背景jobの開始・受理・上限・破棄、cache所有者・寿命、DocumentKey、保存・入力・undo/redo・geometryを変更しない。
- 機能追加、Markdown解釈変更、parse/fallback統合、性能最適化、依存更新、全体整形、フッター、配布、workflowと保護パスの変更は行わない。

着手時に確認したopen PR #538（head `d2f926cc5d0d6996ac9fccec147489115268c8be`）はview.rsのフッター・設定・保存状態などを変更するが、取得したpatchは上記表示入口の呼出しを変更していない。line.rs/shape.rs/basic_editing_keys.rsも同PRの変更ファイル一覧にない。これを将来の無競合の保証とせず、受入前に最新のhead・base・差分を再確認する。未受入のフッター実装を取り込んだり複製したりしない。

## P2の受入条件と検証

1. 通常ビルド・テストとも新しい一入口を使い、旧 `presented_block_with_list_projection` / `presented_block_with_projections` / `presented_block_with_table_projection` の定義・呼出し・不要importが製品Rust sourceに残らない。下位の `present_block_with_table_projection` は名前が異なる既存APIなので撤去しない。
2. 新入口は旧9引数本体と同じ処理であり、各呼出元の旧引数と新fieldの値が一対一で一致する。9fieldの型・借用以外の状態や判定を増やさない。
3. 既存テストを削除・ignore・弱体化せず、wrapper経由のテストも同じfixtureとassertionで新入口を通る。テストの追加が不要な単純移行なら、数を増やすこと自体を目的にしない。
4. current headのworkspace tests、macOS/Windowsの通常CI、Clippyが成功し、最終候補のreviewと差分照合を記録する。過去のP1の成功をP2の実行結果として扱わない。

検証対象には既存のlist/fence/table、検索による強制表示、倍率反映済み行高、画面外marker、巨大block、隣接block移動、P1の参照条件テストを含める。既存の名前・期待値を維持するのでworkspace全体を実行する。

```sh
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
git diff --check
```

対象head、base context、実行コマンド・結果、移行したpathと呼出元対応、未実施事項をPRに残す。Claude workerはRead/Edit/Write/Glob/Grepだけを使い、shell・git・test/build/lint・commit・pushを実行しない。trusted finalizerと通常検証経路の結果を別に確認する。

最終差分がrequest構築・分解と旧wrapper撤去だけで、同じ引数値・関数本体・処理順・解析量を維持すると確認できる場合、P2固有の実画面・性能再測定は不要と判断できる。その判断は最終差分確認後にPRへ記録する。表示・入力・処理量に変更や不明点が残る場合は機械的移行の範囲に戻すか、必要な検証を取得する。Issue #306全体のGUI/性能受入を免除しない。

## 実行判断、残作業、戻し方

このセッションの実行環境には既定の `~/.local/bin/jev` がなく、Jevの応答は取得していない。Commander Policy 2.9の取得不能時の規則に従い、current factsからこの一段階を選ぶ。Jevが判断・承認したとは記録しない。

本P2だけでは#306と親#297をcloseしない。同一snapshotを必要とするparse経路全体の照合、異なるfallbackの理由・契約の整理、Issue全体の実画面・性能を含む受入は残る。

永続データと外部公開APIを変更しない。戻す場合は本PRのrequest型・入口・全呼出元移行を一緒にrevertする。後続が新入口へ依存する場合は後続から逆順に戻す。P1の参照条件集約は独立した前段として維持する。
