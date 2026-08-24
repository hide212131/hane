# ADR-0007: IME Composition モデル

## ステータス

承認済み

## 日付

2026-08-24

## 背景

日本語入力は初期版の必須条件である。

IME 入力では、未確定文字列、選択範囲、下線、確定、キャンセル、変換候補操作が発生する。Markdown 解析や presentation 更新によって IME セッションが破壊されると、日本語文章作成ツールとして成立しない。

## 決定

IME state は `editor` crate が保持し、Document Buffer の revision と接続する。

概念構造は以下とする。

```text
ImeState
├── active: bool
├── base_revision
├── transaction_id
├── original_range: SourceRange
├── original_text
├── current_range: SourceRange
├── marked_text
├── selected_utf16_range
└── cursor_affinity
```

IME composition 中の未確定文字列は、Document Buffer へ反映する。

理由は、画面表示、カーソル、選択、scroll into view、keystroke-to-frame 測定を通常入力と同じ経路に乗せるためである。

composition 更新時は、前回の `replacement_range` を新しい `marked_text` で置換する。

確定時は marked state を解除し、Document Buffer 上の文字列はそのまま確定済みとして扱う。

キャンセル時は、composition 開始時の range と text に戻す。

## Transaction と復元条件

IME composition は単一の編集 transaction として扱う。

composition 開始時に以下を保存する。

- `transaction_id`。
- `base_revision`。
- `original_range`。
- `original_text`。
- `start_anchor` と `end_anchor`。
- 開始時の selection。

composition 更新は同じ `transaction_id` 内の置換として記録する。Undo history に未確定更新の途中経過を個別項目として出さない。

commit 時は、composition 全体を1つの undo 単位として確定する。

cancel 時は、以下の条件を確認する。

1. `ImeState.active == true`。
2. `transaction_id` が現在の active transaction と一致する。
3. `start_anchor` と `end_anchor` が解決できる。
4. composition range 外に、この composition と競合する同期 edit がない。

条件を満たす場合、現在の composition range を `original_text` で置換し、selection を開始時に戻す。

条件を満たさない場合は、IME セッションを強制終了し、Document Buffer の現在内容を正とする。この場合でも panic せず、telemetry / debug log に `ImeCancelConflict` を記録する。

Phase 0 では、composition 中に通常 keyboard command、selection 移動、undo、外部更新が来た場合の扱いを以下に固定する。

| イベント | 扱い |
|---|---|
| composition update | 同じ transaction 内で置換 |
| commit | transaction を確定 |
| cancel | 復元条件を満たせば original に戻す |
| cursor / selection 移動 | 先に composition を commit してから移動 |
| 通常 text input | IME update でなければ先に composition を commit |
| undo / redo | Phase 0 では command 自体を未実装。Phase 1 以降は先に composition を commit |
| 外部更新 | Phase 0 では未対応。Phase 1 以降は競合時に composition を commit または cancel conflict |

composition 中の background result は transaction を変更できない。

## 位置変換

IME API が UTF-16 offset を要求する場合でも、Document Buffer の正式 position は `SourceOffset(byte)` のままとする。

`editor` は composition 対象範囲内で以下の変換を提供する。

```text
UTF-16 offset <-> SourceOffset(byte)
```

この変換は ASCII だけでなく、日本語、絵文字、結合文字を含むテストで検証する。

## Background Update との関係

IME composition 中は、background presentation result が到着しても、以下を守る。

- `ImeState` を破棄しない。
- composition range を古い mapping で上書きしない。
- current revision と一致しない result は破棄する。
- current revision と一致しても、composition 表示に必要な marked range を優先する。

## 結果

日本語入力を通常入力と同じ編集経路で扱える。

一方で、composition キャンセルには開始時の text snapshot または inverse edit が必要になる。Phase 0 ではまず snapshot range を保持し、Undo/Redo model が入る段階で edit history と統合する。

## 検討した代替案

### 未確定文字列を Document Buffer に入れず overlay 表示だけにする

採用しない。

表示、カーソル、選択、スクロール、測定の経路が通常入力と分かれ、IME 固有のバグが増える。

### UTF-16 offset を Document Buffer の正式単位にする

採用しない。

Markdown parser と UTF-8 ファイル保存との境界で不利になる。UTF-16 は IME 境界で明示的に変換する。
