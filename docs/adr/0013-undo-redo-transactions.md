# ADR-0013: Undo/Redo Transaction

## ステータス

承認済み

## 日付

2026-08-25

## 背景

Undo/Redoは表示モデルではなくMarkdown sourceへ適用する必要がある。一文字入力ごとに履歴を作ると文章入力として不自然になり、IMEのcomposition updateを個別履歴にすると未確定文字列がUndo途中に現れる。

## 決定

履歴entryは開始byte offset、削除文字列、挿入文字列、編集前後のselection、編集種別を保持する。Undoは挿入後rangeを削除文字列で置換し、Redoは復元後rangeを挿入文字列で置換する。

通常入力は次の条件を満たす間、同じtransactionへまとめる。

- 前回編集から750 ms以内。
- 前回の編集後selectionと次の編集前selectionが一致する。
- 挿入は直前の挿入末尾に連続し、改行をまたがない。
- Backspaceは直前rangeの左側へ連続する。
- Deleteは同じ開始位置から右側へ連続する。

selection置換、改行、IME commitは独立transactionとする。IME composition updateは履歴へ入れず、commitまたはunmark時にcomposition開始前sourceから確定sourceへの1 transactionを作る。cancelはsourceとselectionを開始前へ戻し、履歴を作らない。

Undo/Redo自身も通常のDocument editを利用してrevisionを進めるが、新しい履歴entryは作らない。Undo後の新規編集はredo stackを破棄する。

## 結果

履歴はsource editだけを保持し、presentation cacheやlayout状態を保持しない。Unicode文字列のbyte境界はText Bufferが検証し、selectionもtransaction単位で復元される。
