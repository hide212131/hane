# Issue #605 実装準備

- 対象: [Issue #605](https://github.com/hide212131/hane/issues/605)
- 段階: 初回実装
- 基準ブランチ: `main`
- 基準SHA: `68c438aea3ce39e12b25aa4ad3e2f5e16294190d`
- 作業branch: `work/issue-605-list-indent`
- 状態: **実装準備のみ。製品コード・テストは未変更、未検証。**

## 目的

次のMarkdownで、外側の箇条書き項目にある番号付き子リストの本文が、同じ親階層の `xyz` より右へ寄りすぎる表示を改善する。

```markdown
- 1. abc
- 2. def
- xyz
```

current mainの `crates/presentation/src/layout.rs` は `LIST_DEPTH_INDENT = 24.0` を使用する。Issue #605の初期目標は `16.0` とし、実GUIで階層が区別できることと本文位置が近づくことを確認する。

## 実装範囲

- 既存のpresentation list depth geometryを調整する。
- 描画、caret、hit test、選択、IMEで共有する既存 `BlockLayout` 経路を使う。
- Markdown原文、parser上のlist構造、SourceMap、保存内容に変更を加えない。
- 受け入れ条件に直接必要なlayout回帰テストを更新・追加する。

## 受け入れ条件

Issue #605本文を正とする。特にネスト段差の縮小、同階層内の本文・折り返し位置の整列、マーカー開示と編集時のsource位置対応、必要なfocused GUI evidenceを確認する。

## 関連Issueと依存

- #126 の通常リスト構造・編集・source mappingを再利用する。#126は完了済み。
- #15 の残作業はtask checkbox。通常リストの本Issueに対する先行条件ではない。
- #19 は縦方向spacingであり、横方向のネスト段差とは責務が異なる。
- #307 は `BlockLayout` の座標契約を整理中だが、現時点で関連実装PRはない。本変更は受け入れ済みgeometry経路上のリスト深さ値だけを調整する。#307から本変更の座標契約へ影響する作業が先にmainへ入った場合はその経路を確認する。#307全体の完了は待たない。

## 未実施の検証

実装前のテスト・GUI確認は行っていない。製品変更後に対象headのlayout tests、必要なRust workspace検証、focused GUI scenarioと画像証拠を確認する。Draft PR作成や実装依頼の投稿だけでIssue完了とはしない。
