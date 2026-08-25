# ADR-0014: GPUI Memory Baselineと空Editor RSS目標

## ステータス

承認済み

## 日付

2026-08-25

## 背景

Phase 0で空文書・起動直後RSSはmedian 64.72 MBとなり、初期目標60 MBを超えた。Phase 1開始条件では、60 MB以内へ下げるか、GPUI baselineとの差を明示して目標を再承認することを要求した。

## 測定

本文、入力handler、metricsを持たず、同じGPUI 0.2.2、window size、release profileで空の`div`だけを描画する`gpui_baseline` exampleを30 process起動した。Mach `task_info(MACH_TASK_BASIC_INFO)`の`resident_size`を初回renderで取得した。

| 対象 | Samples | Median | p95 | p99 / Max |
|---|---:|---:|---:|---:|
| 最小GPUI window | 30 | 59,326,464 | 59,392,000 | 59,637,760 |
| Hane空editor ready | 30 | 64,077,824 | 64,307,200 | 64,339,968 |

Hane固有の増分はmedianで約4.75 MB、max同士の差で約4.70 MBである。

## 決定

GPUI 0.2.2自体が60 MB目標の大部分を占め、Hane固有機能をすべて除いたbaselineのmaxが59.64 MBであるため、Phase 2以降は次の二段階の回帰gateを採用する。

- 最小GPUI baseline: max 60 MB以下。
- Hane空editor ready: max 65 MB以下。
- Hane固有増分: median 6 MB以下。

元の60 MB目標に対する結果はFailのまま記録し、数値をPassへ読み替えない。GPUI更新時にはbaselineとHaneを同じ条件で再測定する。

## 結果

空editorの固定memoryをframework分とHane固有分に分離して監視できる。GPUI baselineが将来低下した場合は、Hane絶対上限も同じ差分を維持して引き下げる。
