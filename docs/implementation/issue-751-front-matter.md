# Issue #751 実装ハンドオフ: Front Matter 対応（1行表示と展開編集、新規ノートと命名の移行）

## 位置づけ

本 PR は実装前の準備 PR である。現時点では未実装・未検証であり、Claude による実装を待つ。
設計の正本は `docs/implementation/issue-751-front-matter-design.md`（同梱）。

## スコープ

設計書の方式で、同一 PR 内の 2 フェーズとして実装する。

### フェーズ 1 — 認識・表示・編集
- バイト 0 の `---` 行から後続の最初の `---` 行までを Front Matter として認識する（設計書 §1 の行照合規則どおり。閉じ行がなければ通常の Markdown）
- `hane-markdown` に `front_matter` モジュールを置き、`NodeKind::FrontMatter` を追加。本文だけを pulldown-cmark に渡し、イベント範囲に本文開始オフセットを加算する（§2）
- メタデータの解釈は単純な `key: value` 行のみ。`date` は `YYYY-MM-DD` の実在日付として検証し、値のソース範囲を持つ（§3）
- 編集モードの通常表示は要約 1 行（日付先頭、他項目最大 2 件＋`+N`、折り返しなし）。要約は開き行に置き、固定ソースアンカー（日付の値の先頭→最初の他項目の値の先頭→開き行の先頭）へ着地する（§4）
- キャレット・選択・IME・検索の現在の一致が Front Matter の正確な範囲に触れている間だけ原文を展開する。開閉は `block_disclosure` の既存経路に載せ、`disclosures_are_current` と高さ推定にも同じ判定を使う（§5）
- 開閉は表示状態のみ。バッファ・改訂番号・dirty・Undo/Redo を変えず、保存・命名ジョブを発行しない（§6）
- プレーンテキストモード（Plain / PlainHighlighted）と閲覧モードの扱いは §7 どおり（プレーンは常時原文、閲覧は要約 1 行）

### フェーズ 2 — 新規ノートと命名
- `new_work_folder_note_heading()` を初期テキストを返す形に置き換え、新規ノートは `---\ndate: "YYYY-MM-DD"\n---\n\n# ` で始まり、キャレットは末尾（`# ` の直後）に置く（§8）
- タイトル同期は Front Matter の有効な date と本文の最初の H1 から `日付_タイトル` を組み立てる。`extract_h1_title` は Front Matter 範囲を除外する。date が欠落・不正・重複なら H1 のみ。Front Matter のない文書は現行動作のまま（§9）
- date の確定編集にも改名を追従させる。同名衝突で確定した stem と導出候補の両方を記録し、追跡の継続・停止を判定する（§9）
- サイドバーの日付バッジ・並び替えはファイル名規則の維持により既存経路で追従する（§10）

## 受入条件

- 設計書末尾の受入条件チェックリストをすべて満たすこと
- 設計書 §11 のテスト表にあるテストをすべて実装すること。特に次を欠かさないこと:
  - Markdown: `front_matter_recognizes_only_completed_document_prefix`、`front_matter_keeps_body_event_offsets_and_isolates_metadata`、`front_matter_metadata_uses_one_valid_date`
  - 索引: `front_matter_index_reclassifies_after_delimiter_edit`
  - Presentation: `front_matter_summary_has_one_row_and_stable_source_anchor`、`front_matter_disclosure_respects_exact_boundaries`
  - GPUI: `front_matter_click_and_up_expand_at_the_same_source_offset`、`front_matter_find_replace_copy_and_plain_mode_share_source`
  - Session: `front_matter_title_sync_uses_body_h1_and_date`
  - GPUI: `new_work_folder_note_starts_with_front_matter`、`front_matter_date_rename_tracks_collisions_and_latest_edit`、`front_matter_rename_refreshes_sidebar_badge_and_order`
- `cargo test --workspace --all-targets --no-fail-fast` と `cargo check --workspace`、`cargo fmt --all -- --check` が通ること
- 通常文書（Front Matter のない文書）の本文編集で、先頭ブロックや文書全体の再解析が増えないこと（§11 の性能確認）

## 対象外

- 既存の `# 日付_タイトル` ノートの一括移行・自動書き換え
- フル YAML の導入（配列・マップ・複数行スカラー等の解釈）
- 別ペインや専用フォームでのメタデータ編集
- サイドバーの日付バッジの抽出規則そのものの変更

## 注意

- 設定値を切り替えるテストを書く場合は、プロセス共有の実設定ストアではなく `StateStores::memory()` 等の隔離されたストアを使うこと（過去に共有ストア汚染で大量のテストが連鎖失敗した事故がある）
- 検証は必ず `--no-fail-fast` で全ターゲットを回し、一部ターゲットの結果だけで合否を判断しないこと
- `#[cfg(test)]` 限定の import を本番コードから使わないこと（lib テストだけ通ってワークフローの verify で落ちる事故がある）。ワークスペース全体のビルドが通ることを確認すること
