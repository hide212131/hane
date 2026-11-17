# Issue #743 実装ハンドオフ: サイドバーの日付を完全な右寄せに

## 位置づけ

本 PR は実装前の準備 PR である。現時点では未実装・未検証であり、Claude による実装を待つ。
設計の正本は `docs/implementation/issue-743-date-align-design.md`（同梱）。

## スコープ

設計書 §2 の方式で、通常ツリーのファイル行の末尾に共有領域を設ける。

- 共有領域: 高さ 24px、幅 = max(日付の自然幅, 24px)、`relative + flex_none`、右寄せ・上下中央
- 日付は通常フローに残し、ホバー時は `invisible()`（幅は維持）
- ピンは既存 `pin_toggle_slot` を共有領域内の右端へ絶対配置するラッパー経由で置く（要素自身には絶対配置を付けない）
- 状態表（設計書 §2）どおり: ピン済み行も非ホバーでは日付、リネーム中は日付なし＋24px 枠、`pin_relative=None` ではホバーしても日付を消さない
- `file_row_content` の最終引数を共有末尾要素に変更し、`file_name_label` と `work_folder_files_sidebar` のファイル分岐を設計書 §3 の分割で組み替える

## 受入条件

- 設計書 §5 のチェックリストをすべて満たすこと
- 設計書 §4 のテスト更新・追加を行うこと。特に:
  - `date_badge_text_aligns_to_row_padding_edge`: 非ホバー時の日付右端が行パディング内側に一致（許容誤差 0.5px）
  - `file_row_hover_swaps_date_and_pin_without_relayout`: ホバーで日付とピンが排他的に切り替わり、名前・共有領域・日付の bounds が不変
  - `pinned_file_row_restores_date_after_hover`、`file_row_without_date_keeps_pin_reachable`、`file_row_trailing_pin_click_does_not_open_or_rename`、`file_row_name_click_opens_file_with_shared_trailing_slot`、`file_date_stays_visible_when_pin_target_is_unavailable`、`inline_rename_keeps_shared_pin_slot_and_blocks_toggle`
  - 旧基準のテスト（`assert_date_aligned_to_pin_slot` 等）は新基準へ更新する
- 表示切替の検証は、非表示になる親の子孫に置いたテスト専用 selector で行う（親自身の bounds は可視性の証拠にならない、という設計書の注意に従う）
- `cargo test -p hane-ui --no-fail-fast` と `cargo check --workspace` が通ること

## 対象外

- フォルダ行・ルート行・独立した「ピン留め」領域の行のレイアウト変更
- ピンの永続化・並び順・保存中の制御の変更
- 日付抽出・相対表記・濃度（#719 仕様）の変更

## 注意

- 設定値を切り替えるテストを書く場合は、プロセス共有の実設定ストアではなく `StateStores::memory()` 等の隔離されたストアを使うこと（過去に共有ストア汚染で大量のテストが連鎖失敗した事故がある）
