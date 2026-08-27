# ADR-0020: ブロック単位の仮想化と描画

## ステータス

承認済み

## 日付

2026-08-28

## 背景

R3.5 までの描画は「1物理行 = 1描画単位」だった。`EditorView::render` は可視行を
反復し、行ごとに `VisualBlock`（実体は1行）を presentation から得て、行ごとに GPUI 要素を
作る。`HeightIndex` も1行1エントリだった。

この形は Markdown のブロック構造と噛み合っていない。

- fenced code / table が「行の性質」として扱われる。`EditorView` は
  `BlockContextIndex`（全文の行走査）と `local_block_context`（viewport 周辺の行走査）を
  持ち、行ごとに `line_is_fenced` / `line_is_table` を引いて `LineContext` を組み立てていた。
  R3.5 で `BlockIndex` が入った後もこれが残り、ブロック境界を二重に持っていた。
- 複数行ブロックのレイアウト（R4B）とブロック単位の差分キャッシュ（R4C）が、
  行単位の描画の上には載らない。

R4A の目的は、カーソル・選択・IME の挙動を変えずに、仮想化と要素生成の単位だけを
Markdown ブロックへ移すことにある。

## 決定

### 型を「ブロック」と「行」に分ける

`hane_presentation` の従来の `VisualBlock`（1物理行）を `VisualLine` へ改名し、
`VisualBlock` を Markdown ブロックの単位として作り直す。`VisualBlock` は
`BlockId` / 表示種別 / 複数行にまたがる `source_range` / revision / `Confidence` と、
その中の `VisualLine` を持つ。R4A の互換レイヤはこの `lines` であり、R4B が
`LayoutLine` へ置き換える。

### 表示文脈はブロック種別だけから決める

`presentation::block_line_context(NodeKind) -> LineContext` を唯一の判定点とし、
`BlockContextIndex` / `parse_block_context` / `LocalBlockContext` /
`local_block_context` と、その土台の行走査（`scan_block_context`、`is_pipe_row`）を削除する。
`fence_delimiter` はマーカ導出の内部関数として残す。

ブロック間の空行はタイル化により上のブロックへ属するため、ブロック末尾の空行列だけは
`LineContext::Normal` で描く。閉じ fence の直後の空行がコード背景で塗られないための規則で、
コードブロック内部の空行はコードのまま扱われる。

### 正式索引が無い間は行単位で仮想化する

文書全体の `BlockIndex` は全文解析を要し、100 MB では約1.2秒かかる。起動をこれで
待たせないため、`heights` が何を数えるかを2状態にする。

- `Granularity::Blocks`: 正式/暫定の `BlockIndex` が現 revision を説明している間。
  1エントリ = 1ブロック。
- `Granularity::Lines`: 索引が無い間（起動直後、編集履歴が途切れて索引を落とした後）。
  1エントリ = 1物理行。

どちらの状態でも描画されるのはブロックである。行単位のときはブロック境界を
`hane_markdown::local_block_index` から取る。これは viewport の上に固定行数の lookback を
取った窓を `parse_document` で解析してタイル化するもので、行走査版 `local_block_context` の
置き換えにあたる。全ブロックが `Confidence::Provisional` である。

`heights` の入れ替え時は viewport 上端の source offset を控えて掛け直すため、
粒度が切り替わっても読んでいる位置は動かない。

### ブロックは可視行でクリップする

ブロックに大きさの上限は無い。空行を含まない文書は CommonMark では1段落であり、
実際 `paragraphs_100k.md`（10万行）は1ブロックになる。ブロック全体を描くと要素生成が
文書サイズに比例してしまうため、`present_block` は viewport と交差する行だけを構築し、
その上下の行数だけを数えて `lines_before` / `lines_after` として保持する。ブロック高さは
「描いた行の実測 + 描かなかった行 × 行高」で、描画状態に依存しない。

### 行数は索引が持つ

ブロック高さの初期値は行数から決まる。行数を rope から引き直すと 10万ブロックで 20 ms を
超え、ブロック数が変わる編集（改行の入力）ごとに入力経路へ乗る。タイル化の時点では
ブロックの bytes が手元にあるので、そこで数えて `IndexedBlock::line_count` として持つ。

行数は「ブロック内の改行数 + 末尾が改行でなければ1」と定義する。この定義は連結に対して
加法的なので、ブロックの併合で数え直しが要らない。末尾の改行が作る空の最終行は
どのブロックにも数えられないため、`block_heights` が文書の行数と突き合わせて最後の
ブロックへ足す。

### 正式索引公開時の高さ再構築は背景で行う

`block_heights` と `HeightIndex::new` はブロック数に比例する。100 MB（約265万ブロック）で
合わせて約39 ms になるため、全文解析と同じ背景 job で作って main thread では差し替えるだけに
する。解析中の編集で rebase が起きてブロック数がずれた場合だけ、main thread で組み直す。

## 結果

- GPUI 要素は可視ブロック数に比例し、ブロック内では可視行数に比例する。
- `crates/ui` はフェンスやパイプを見ない。表示文脈はブロック種別からのみ決まる。
- ブロック境界の二重管理が無くなり、`BlockIndex` が唯一の境界になった。
- 10万ブロックの高さ索引再構築は 21.1 ms から 0.635 ms になった（`hane-bench buffer` の
  `block height index rebuild`）。行単位だった従来の再構築（20万行、約1.5 ms）より速い。
- カーソル・選択・IME・クリックは物理行を指したままで、R0.5 の契約テストは変更していない。
  同一文書を master と R4A で描画し、本文領域の表示が一致することを画面キャプチャで確認した。
- 一方で、ブロック内の行位置とスクロール位置の対応は行高一定を仮定している。画像行のように
  高さの異なる行があるブロックでは可視行の見積もりが1〜2行ずれうる。overscan が吸収するが、
  正確な対応は R4B の `LayoutLine` で持つ。

## 検討した代替案

### 起動時に `BlockIndex` を同期構築する

採用しない。100 MB で約1.2秒を起動へ積む。R0 の startup gate を大きく超える。

### `HeightIndex` を物理行のまま残し、描画だけブロックにする

採用しない。R4C の差分更新とスクロール anchoring はブロック単位の高さの上に載る。
また行単位のままでは 100 MB で 530 万エントリの Fenwick を持ち続けることになる。

### ブロック全体を常に present する

採用しない。1ブロック = 文書全体になる文書が実在する（空行の無い文書）。
仮想化の単位がブロックでも、構築の単位は可視行でなければならない。

### 行数を rope から引き直す

採用しない。10万ブロックで 20 ms 超、ブロック数が変わる編集ごとに入力経路へ乗る。

### 空行かどうかで表示文脈を決める

採用しない。コードブロック内部の空行がコード背景を失う。ブロック末尾の空行列だけを
除く規則が要る。
