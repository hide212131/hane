# ADR-0003: Text Buffer と位置単位

## ステータス

承認済み

## 日付

2026-08-24

## 背景

100 MB 級の Markdown 文書では、1文字入力のたびに文書全体をコピーする構造は成立しない。

また、本エディタは Markdown ソースを唯一の正とし、カーソル、選択、IME、Undo/Redo、Markdown 解析を同じ source position 体系で扱う必要がある。

## 決定

Phase 0 では Text Buffer を抽象化し、実装候補として Rope を第一候補にする。

ただし、Phase 0 の目的は最終データ構造の美しさではなく、入力遅延と巨大文書挙動の測定である。そのため、以下の interface を満たす実装で開始する。

```rust
TextBuffer
├── len_bytes(&self) -> ByteLen
├── len_chars(&self) -> CharLen
├── revision(&self) -> Revision
├── validate_offset(&self, offset: SourceOffset) -> Result<(), BufferError>
├── validate_range(&self, range: SourceRange) -> Result<(), BufferError>
├── slice(&self, range: SourceRange) -> Result<BufferSlice<'_>, BufferError>
├── text(&self, range: SourceRange) -> Result<String, BufferError>
├── edit(&mut self, range: SourceRange, replacement: &str) -> Result<EditSummary, BufferError>
├── line_for_offset(&self, offset: SourceOffset) -> Result<LineId, BufferError>
├── offset_for_line_col(&self, line: LineId, col: LineCol) -> Result<SourceOffset, BufferError>
└── anchor(&self, offset: SourceOffset, bias: Bias) -> Result<Anchor, BufferError>
```

正式な source position は UTF-8 byte offset とする。

理由は以下である。

- Markdown parser の source range と接続しやすい。
- ファイル保存時の Markdown ソースと対応しやすい。
- ASCII、Markdown 記号、日本語を含む文書で、parser の range と同じ単位を使える。

ただし、UI と IME では UTF-16 位置や grapheme cluster が必要になる。そのため、`editor` 層で以下の変換を明示的に扱う。

```text
SourceOffset(byte)
Utf16Offset
CharOffset
GraphemePosition
VisualPosition
```

これらを暗黙の `usize` として混在させない。型 alias または newtype で区別する。

## SourceOffset と SourceRange の契約

`SourceOffset` は UTF-8 byte offset を表す newtype とする。

`SourceRange` は半開区間 `[start, end)` とする。

以下を不変条件とする。

- `0 <= start <= end <= len_bytes`。
- `start` と `end` は UTF-8 文字境界でなければならない。
- `SourceRange` は byte range であり、char range でも grapheme range でもない。
- 空 range は挿入位置として有効である。
- 改行は buffer 内の実バイト列を保持し、内部正規化しない。
- 行分割は CommonMark 0.31.2 の source line ending 定義に従い、`\n`・`\r\n`・孤立した `\r` をそれぞれ1つの行境界として扱う。`\r\n` は2 byte の列のまま1つの改行として扱う。

`TextBuffer` の line query (`line_count`、offset→line、line+column→offset、`line_range`、`line_content_range`) はこの3種の line ending すべてで同じ意味を返す。Markdown parser（pulldown-cmark 経由）はこの3種をもともと source line ending として扱っており、editor 側の line 概念を parser 側の block/line 分割と一致させるための決定である。

保存 bytes は正規化しない。`RopeBuffer` の実装は Ropey の line index が `\n` のみを改行とみなす制約（`unicode_lines` を無効化した設定、100 MB 級文書での性能測定のため）に対し、`rope` と byte/char 位置が1:1で対応する内部専用の mirror rope を保持し、孤立した `\r` をその mirror 上でのみ `\n` に置き換えて Ropey の line index を再利用する。mirror は通常 edit のたびに、その edit の範囲と直前1文字分の再分類だけを更新し、文書全体を再走査しない。保存・読み取り・`text()`/`slice()` は常に実バイトを保持する `rope` 側から行い、mirror の内容が外部に漏れることはない。

### 2026-08-24 時点の決定との違い

当初は「孤立した `\r` は通常文字として扱う」としていたが、これは Ropey の line index の制約をそのまま editor の仕様に持ち込んだものであり、CommonMark の source line ending 定義とも、pulldown-cmark が実際に解釈する block/line 分割とも矛盾していた。この不一致は caret・selection・IME・SourceMap・保存再読込の広い範囲で bare CR 付近の表示・カーソル位置のずれとして表面化したため、上記の通り改めた。

`slice`、`text`、`edit`、`line_for_offset`、`offset_for_line_col`、`anchor` は、境界外 offset または UTF-8 文字境界外 offset を受け取った場合に panic せず `BufferError` を返す。

`replacement` は有効な UTF-8 `&str` のみを受け取る。したがって、edit 後の buffer は常に有効な UTF-8 である。

`BufferError` は最低限以下を持つ。

```rust
BufferError
├── OffsetOutOfBounds { offset, len }
├── RangeOutOfBounds { range, len }
├── InvalidRange { range }
├── NotCharBoundary { offset }
└── InvalidLineColumn { line, col }
```

`BufferSlice<'a>` は rope 実装の内部 chunk lifetime を隠す facade とする。

Phase 0 では `slice()` を高速な読み取り用、`text()` を所有 `String` が必要な境界用として分ける。UI や parser が長期保持する文字列には `BufferSlice<'_>` を直接持たせず、必要な範囲だけ snapshot または owned text に変換する。

## Anchor の移動規則

`Anchor` は `SourceOffset` と `Bias` を持つ。

```text
Bias
├── Before
└── After
```

edit による anchor 移動規則は以下とする。

| edit と anchor の関係 | 移動規則 |
|---|---|
| edit range より前 | 変化しない |
| edit range より後 | `replacement.len_bytes - deleted.len_bytes` だけ移動 |
| 削除 range 内 | replacement の境界へ移動 |
| 挿入位置と同じ offset / `Before` | 挿入テキストの前に残る |
| 挿入位置と同じ offset / `After` | 挿入テキストの後へ移動 |

削除 range 内の anchor は、`Before` なら replacement の先頭、`After` なら replacement の末尾に移動する。

## Phase 0 の実装条件

Phase 0 の Text Buffer は以下を満たす。

- 通常入力で文書全体をコピーしない。
- 10 MB / 100 MB 文書の任意位置挿入・削除を測定できる。
- 行頭、行末、任意 offset への移動を実装できる。
- revision を edit ごとに単調増加させる。
- IME composition 中の置換 range を表現できる。

## 結果

Markdown parser、presentation、editor が同じ source offset を参照できる。

UTF-8 byte offset を正にすることで parser 連携は単純になるが、ユーザー操作では UTF-16 と grapheme を扱う必要がある。変換層のテストが重要になる。

## 検討した代替案

### 単一の Rust `String`

採用しない。

巨大文書の途中編集でコピー量が文書サイズに比例しやすく、RFP の性能要求と矛盾する。

### UTF-16 offset を正式 position にする

採用しない。

IME とは接続しやすいが、Markdown parser、UTF-8 ファイル保存、Rust 文字列 slice との境界で変換負荷とバグが増える。

### Grapheme cluster を正式 position にする

採用しない。

ユーザー操作には必要だが、Markdown source range の正規単位としては重すぎる。grapheme は editor / presentation の表示側で扱う。
