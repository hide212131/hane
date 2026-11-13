# Issue #719 実装引き継ぎ（フェーズ p1）

## 対象

- Issue: #719「サイドパネルの日付タグを周囲と同等か薄いテキスト表示にして右寄せする」
- フェーズ: p1（本 Issue の実装全体。フェーズ分割なし）
- 基準 SHA（準備 commit の親、current main）: `b06d8648375c3fd76b7c1c8d6b2e7e01edb0cd1b`
- 採用済み設計: `docs/implementation/issue-719-date-badge-design.md`（本 PR に同梱）

## 状態

**未実装・未検証。** この文書と設計書の追加だけが準備 commit であり、製品コードとテストはまだ変更されていない。実装はこの PR への Commander ハンドオフを受けた Claude Code worker が行う。

## 今回の範囲

設計書の確定事項（冒頭の司令塔追記を含む）に従う。要点:

1. `crates/ui/src/view/sidebar.rs`: `date_badge_chip` を廃し、日付を 10px のテキストで表示する。色は `theme.sidebar_foreground` の不透明度で、本日は 1.0、それ以外は 0.7。背景定数（`DATE_BADGE_*`）と `date_badge_background` は削除する。
2. 右寄せの行構造の組み替え: 日付をインデント付き内容の外（名前領域の兄弟）に置き、名前領域（`flex_1` / `min_w(0)`）が先に縮み、名前は truncate され、日付は欠けない。日付の右端はピンスロット（24px、常時確保）の左境界に揃い、ホバーで動かない。日付だけの名前でも空の名前領域を残して右寄せする。
3. `DateBadgePosition` と `badge_renders_before_remainder`、位置引数と左右順序のテストを削除する。
4. 設計書のテスト計画（a11y 関連を除く）に基づくテストの追加・置換。

## 対象外

- a11y の行名付与（`Role::Group` / `aria_label`）: フォローアップ Issue #720。関連テスト `file_row_accessible_label_matches_untruncated_display_parts` も実装しない
- 日付の抽出規則・相対日付の文面・近さ分類（`crates/session/src/date_badge.rs`）の変更
- テーマへの新しい色フィールドの追加（既存 `sidebar_foreground` の不透明度で表す）
- ピン操作（`sidebar_pins.rs`）の表示・領域・イベント処理の変更
- 日付の文字サイズの変更（10px を維持）

## 受入条件の柱

- 日付はチップではなくテキストで表示され、本日のみ不透明度 1.0、それ以外は 0.7。
- 両テーマ × 通常／アクティブ行で、合成後のコントラスト比が 4.5:1 以上（テストで固定）。
- 短名・長名・日付のみ・深い階層・最小幅 160px で、日付が右端に揃い全体が欠けない。名前は truncate される。
- ピンのホバーで日付位置が不変。リネーム中は日付が隠れ、確定・取消で復帰する。日付部分のクリックは行の操作として届く。
- 既存テスト `date_badge_palette_has_visible_steps_and_readable_light_text` は削除し、新仕様のテストに置き換える。session 側の date_badge テストは変更せず再実行する。
