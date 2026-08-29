# ADR-0010: Phase 0 実装順序

## ステータス

承認済み

## 日付

2026-08-24

## 背景

Phase 0 は技術検証であり、完成品の縦切りではない。実装順序を誤ると、UI の見た目や Markdown 対応に時間を使い、最重要リスクである入力遅延と IME を後回しにしてしまう。

## 決定

Phase 0 は以下の順序で実装する。

1. GPUI dependency の version / commit を固定する。
2. 最小 GPUI spike を作り、日本語 IME event、marked text、selected UTF-16 range、paint callback 相当の観測点が取れるか確認する。
3. Cargo workspace と crate 境界を作る。
4. `document` に Text Buffer interface、revision、basic edit、anchor、IME cancel 用 snapshot / inverse edit を実装する。
5. `benchmark` に fixture generator と buffer edit benchmark を実装する。
6. `app` と `ui` に最小 GPUI window を実装する。
7. GPUI input event を `editor` command に接続する。
8. カーソル、単一選択、テキスト入力を実装する。
9. IME composition transaction を実装する。
10. visible range だけを描画する scroll model と `HeightIndex` を実装する。
11. 太字だけの presentation 実験を追加する。
12. `keystroke_to_frame`、`keystroke_to_model`、frame time、memory を測定する。
13. Phase 0 report を作成し、Phase 1 へ進めるか判断する。

この順序では、Markdown 表示より前に buffer、input、IME、scroll、measurement を成立させる。

## Phase 0 での非目標

以下は Phase 0 の作業に入れない。

- ファイル保存 UI。
- Markdown の完全な block parser。
- Source ↔ Visual mapping の完成。
- Undo/Redo の完成。
- Undo/Redo command。
- テーマや設定 UI。
- 配布用 packaging。

## 結果

最重要リスクを早い段階で露出できる。

Phase 0 で見た目の完成度は低くなるが、製品実装へ進めるかどうかの判断材料は得られる。

## 完了条件

Phase 0 は以下が揃った時点で完了とする。

- 10 MB / 100 MB fixture を開いた状態で入力できる。
- 日本語 IME composition と確定入力ができる。
- スクロール中に可視範囲のみ描画できる。
- 太字だけの presentation 更新ができる。
- ADR-0009 の測定値を出せる。
- 測定結果と次フェーズ判断をまとめた report がある。
