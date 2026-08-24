# Phase 0 実装・測定レポート

## 実装結果

ADR-0010 の順序に沿って、以下を実装した。

- GPUI `0.2.2` と Rust `1.93.1` を固定。Metal Toolchain がない開発環境では `runtime_shaders` を使用。
- `app`、`document`、`markdown`、`presentation`、`editor`、`ui`、`benchmark` の Cargo workspace。
- `ropey` による UTF-8 byte offset の Text Buffer、revision、edit summary、inverse edit、anchor、revision delta。
- grapheme 単位の cursor、単一 selection、通常 text input。
- UTF-16 ↔ UTF-8 変換を含む IME composition transaction、commit、cancel、競合検出。
- 太字だけの局所 parse、hidden marker を持つ SourceMap、bold style run。
- Fenwick tree の `HeightIndex`、overscan 付き visible range、scroll anchoring primitive。
- GPUI `EntityInputHandler`、未確定範囲、選択 UTF-16 range、paint callback への入力 latency 観測点。
- 10 MB / 100 MB / 100,000段落 / 日本語 / Unicode混在 fixture generator。
- median / p95 / p99 / max、環境情報、RSS、buffer edit、file open、presentation、layout の harness。

Phase 0 の非目標である保存UI、完全なMarkdown parser、Undo/Redo command、設定、packaging は実装していない。

## 自動測定

測定日: 2026-08-24。Apple M3 Pro、macOS 26.5.1、release profile、Rust 1.93.1、GPUI 0.2.2。各buffer editは先頭・中央・末尾を循環し90 samples、file openは30 samples。fixtureは測定直前に自動生成した。

| Scenario | Samples | Median (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---|---:|---:|---:|---:|---:|
| 10 MB buffer edit | 90 | 0.001 | 0.003 | 0.020 | 0.020 |
| 100 MB buffer edit | 90 | 0.000 | 0.002 | 0.009 | 0.009 |
| 10 MB file open | 30 | 12.047 | 12.676 | 14.388 | 14.388 |
| 100 MB file open | 30 | 69.889 | 71.643 | 93.728 | 93.728 |
| bold presentation update | 1,000 | < 0.001 | < 0.001 | < 0.001 | 0.005 |
| visible layout index | 1,000 | < 0.001 | < 0.001 | < 0.001 | < 0.001 |

測定プロセスの100 MB scenario後RSSは238,108,672 bytesだった。この値にはallocatorが保持した以前のscenarioの領域も含まれるため、ADR-0001の「100 MB文書 <= 350 MB」に対する保守的な参考値として扱う。

## UI測定点

アプリは起動時に `startup_time_ms` と `file_open_time_ms` を標準エラーへ出力する。入力ごとに `input received → model updated` を記録し、その編集を含む `InputCapture::paint` で `keystroke_to_frame` を確定する。直近のp95はウィンドウ上部に表示する。paint間隔、visible layout時間、RSS取得APIも実装済みである。

## 検証結果

- `cargo check --workspace`: pass
- `cargo clippy --workspace --all-targets -- -D warnings`: pass
- `cargo test --workspace`: 15 tests pass
- GPUI window smoke launch: pass
- fixture byte size: 10,485,760 bytes / 104,857,600 bytes
- `paragraphs_100k.md`: 100,000 lines

単体テストはUTF-8境界、CRLF、anchor bias、inverse edit、revision rebase、日本語・絵文字・結合文字・サロゲートペア、IME update / commit / cancel、bold SourceMap、HeightIndexを対象とする。

## Phase 1 判断

現時点の判断は **保留** とする。buffer edit、file open、局所presentation、visible range indexは十分小さい値だが、ADR-0001の合格判断には実IMEを使った30回以上の `keystroke_to_frame`、cold / warm startup、100 MB文書でのscroll frame time、load直後・visible layout後・30秒idleのRSSを同一手順で採取する必要がある。

手動検証では次を固定する。

1. `cargo run --release -p hane -- target/fixtures/markdown_100mb.md` を起動する。
2. 文書の先頭・中央・末尾でASCII入力と日本語IME composition / commit / Escape cancelを各30回行う。
3. scrollのみ、scroll中の入力、background presentation更新中の入力を各30秒計測する。
4. 上部のframe p95、標準エラーのstartup / file open、RSSを記録する。
5. ADR-0001の閾値と比較し、Phase 1の Go / Hold を更新する。

