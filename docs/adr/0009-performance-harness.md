# ADR-0009: Performance Harness と測定基準

> **Amended (R5):** 現行の `metrics` / `benchmark` 分離と統合 script は
> [architecture](../architecture.md) および baseline 文書を正とする。

## ステータス

承認済み

## 日付

2026-08-24

## 背景

本製品の成功条件は「高速に感じること」ではなく、巨大文書でも入力が止まらないことを継続的に測定できることである。

平均値が良くても p95 / p99 で 100 ms 停止するエディタは不合格である。

## 決定

`benchmark` crate に性能測定 harness を置く。

Phase 0 から以下を測定する。

| 指標 | 内容 |
|---|---|
| `startup_time` | プロセス開始から入力可能になるまで |
| `file_open_time` | fixture 読み込み開始から編集可能になるまで |
| `keystroke_to_frame` | input event から該当文字を含む frame の paint 呼び出しまで |
| `keystroke_to_model` | input event から該当文字が paint 対象 model に反映されるまで |
| `frame_time` | スクロール中の frame interval |
| `parse_time` | presentation / parse job の処理時間 |
| `layout_time` | visible block layout の処理時間 |
| `memory_usage` | 条件ごとのプロセスメモリ |

測定結果は median、p95、p99、max を出す。

`keystroke_to_frame` を主 KPI とする。`keystroke_to_model` は補助指標であり、実画面反映の代替として合否判定に使わない。

Phase 0 の測定 fixture は自動生成する。

```text
target/fixtures/
├── markdown_10mb.md
├── markdown_100mb.md
├── paragraphs_100k.md
├── japanese.md
└── unicode_mixed.md
```

fixture は Git に含めず、生成器を Git に含める。

## 再現条件

Phase 0 benchmark は以下の条件を記録する。

- git commit hash。
- build profile。
- Rust toolchain。
- GPUI version または commit。
- OS version。
- CPU / memory。
- display refresh rate。
- fixture name と byte size。
- background job の有無。

各 benchmark は原則として warmup 後に30回以上測定する。

startup の cold start は OS cache の影響を完全には排除できないため、測定手順を report に明記し、同一手順で比較する。

入力シナリオは最低限以下を固定する。

- ASCII 連続入力。
- 日本語 IME composition から commit。
- 100 MB 文書の先頭、中央、末尾付近での入力。
- scroll 中の入力。
- background presentation job 実行中の入力。

## Keystroke-to-frame

最重要指標は `keystroke_to_frame` とする。

測定点は以下に置く。

```text
input event received
  -> document edit applied
  -> presentation invalidated / updated
  -> GPUI paint scheduled
  -> paint callback for frame containing the edit
```

Phase 0 では GPUI の正確な display presentation completion までは要求しない。

ただし、`paint target updated` は `keystroke_to_model` として別名で記録し、`keystroke_to_frame` と混同しない。

## Memory

メモリ測定は OS により差が出るため、測定方法を固定する。

Phase 0 の基準環境は macOS / Apple Silicon とし、同じ方法で継続測定する。

Phase 0 では、メモリは同一プロセスで fixture load 完了後、最初の visible layout 完了後、30秒 idle 後の3点で記録する。

## 結果

性能劣化を主観ではなく数値で検出できる。

一方で、benchmark 実装自体が開発コストになる。Phase 0 では最小限の計測から始めるが、`keystroke_to_frame` だけは必須とする。

## 検討した代替案

### 手動操作だけで性能確認する

採用しない。

p95 / p99 の遅延や回帰を検出できない。

### 完成後に benchmark を追加する

採用しない。

性能が設計目標であるため、測定は実装開始時点から必要である。
