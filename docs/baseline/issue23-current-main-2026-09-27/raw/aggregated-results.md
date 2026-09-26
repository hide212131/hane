# UI measurement results

- Git: `174558e522feb7ed17e9cc7567dcd71e0499a7d6`
- Profile: `release`
- Rust: `rustc 1.93.1 (01f6ddf75 2026-02-11) (Homebrew)`
- GPUI: `gpui-pre 0.3.6`
- OS: `26.6.2`
- CPU: `Apple M3 Pro`
- Input sources: `com.apple.keylayout.ABC`
- Refresh rate: `variable (CGDisplayMode reports 0)` Hz
- Background job states: `false`

| Scenario / metric | Samples | Median | p95 | p99 | Max | Unit |
|---|---:|---:|---:|---:|---:|---|
| 100 MiB document warm startup — frame_interval | 55 | 16.170 | 19.904 | 20.090 | 20.090 | ms |
| 100 MiB document warm startup — layout | 115 | 16.543 | 18.259 | 20.994 | 21.063 | ms |
| 100 MiB document warm startup — startup | 60 | 372.127 | 390.542 | 407.671 | 407.671 | ms |
| 100 MiB document warm startup — file_open | 60 | 235.174 | 244.397 | 248.879 | 248.879 | ms |
| 100 MiB document warm startup — memory_load | 60 | 350339072.000 | 350633984.000 | 352550912.000 | 352550912.000 | bytes |
| 10k work folder warm startup — frame_interval | 170 | 52.826 | 407.897 | 426.635 | 429.761 | ms |
| 10k work folder warm startup — layout | 230 | 0.111 | 20.305 | 21.009 | 21.390 | ms |
| 10k work folder warm startup — startup | 60 | 187.074 | 204.975 | 229.663 | 229.663 | ms |
| 10k work folder warm startup — file_open | 60 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| 1k work folder warm startup — frame_interval | 236 | 25.792 | 86.912 | 102.556 | 107.692 | ms |
| 1k work folder warm startup — layout | 296 | 0.126 | 2.877 | 3.490 | 3.528 | ms |
| 1k work folder warm startup — startup | 60 | 189.263 | 210.459 | 219.488 | 219.488 | ms |
| 1k work folder warm startup — file_open | 60 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty cold startup (OS cache not purged) — frame_interval | 58 | 12.057 | 16.033 | 32.562 | 32.562 | ms |
| empty cold startup (OS cache not purged) — layout | 118 | 0.111 | 0.137 | 0.153 | 0.170 | ms |
| empty cold startup (OS cache not purged) — startup | 60 | 185.172 | 199.390 | 231.465 | 231.465 | ms |
| empty cold startup (OS cache not purged) — file_open | 60 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty cold startup (OS cache not purged) — memory_ready | 60 | 71417856.000 | 71581696.000 | 71614464.000 | 71614464.000 | bytes |
| empty warm startup — frame_interval | 54 | 12.886 | 20.924 | 28.306 | 28.306 | ms |
| empty warm startup — layout | 114 | 0.113 | 0.139 | 0.386 | 0.406 | ms |
| empty warm startup — startup | 60 | 187.183 | 227.768 | 312.420 | 312.420 | ms |
| empty warm startup — file_open | 60 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty warm startup — memory_ready | 60 | 71467008.000 | 71729152.000 | 71876608.000 | 71876608.000 | bytes |
| empty work folder warm startup — frame_interval | 231 | 21.404 | 63.017 | 75.320 | 86.410 | ms |
| empty work folder warm startup — layout | 291 | 0.069 | 0.128 | 0.142 | 0.153 | ms |
| empty work folder warm startup — startup | 60 | 188.444 | 214.452 | 235.544 | 235.544 | ms |
| empty work folder warm startup — file_open | 60 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty work folder warm startup — memory_ready | 60 | 71467008.000 | 71581696.000 | 71696384.000 | 71696384.000 | bytes |
| memory 1 MiB — frame_interval | 6 | 18.252 | 136.945 | 136.945 | 136.945 | ms |
| memory 1 MiB — layout | 8 | 0.231 | 7.404 | 7.404 | 7.404 | ms |
| memory 1 MiB — startup | 2 | 238.392 | 243.757 | 243.757 | 243.757 | ms |
| memory 1 MiB — file_open | 2 | 79.737 | 80.260 | 80.260 | 80.260 | ms |
| memory 1 MiB — memory_idle_30s | 2 | 121995264.000 | 122175488.000 | 122175488.000 | 122175488.000 | bytes |
| memory 1 MiB — memory_load | 2 | 70959104.000 | 71024640.000 | 71024640.000 | 71024640.000 | bytes |
| memory 1 MiB — memory_visible_layout | 2 | 83656704.000 | 83755008.000 | 83755008.000 | 83755008.000 | bytes |
| memory 10 MiB — frame_interval | 10 | 17.789 | 134.513 | 134.513 | 134.513 | ms |
| memory 10 MiB — layout | 12 | 0.198 | 6.547 | 6.547 | 6.547 | ms |
| memory 10 MiB — startup | 2 | 214.714 | 222.001 | 222.001 | 222.001 | ms |
| memory 10 MiB — file_open | 2 | 88.981 | 90.533 | 90.533 | 90.533 | ms |
| memory 10 MiB — memory_idle_30s | 2 | 317440000.000 | 371064832.000 | 371064832.000 | 371064832.000 | bytes |
| memory 10 MiB — memory_load | 2 | 95895552.000 | 95911936.000 | 95911936.000 | 95911936.000 | bytes |
| memory 10 MiB — memory_visible_layout | 2 | 110477312.000 | 110493696.000 | 110493696.000 | 110493696.000 | bytes |
| memory 100 MiB — frame_interval | 4 | 20.687 | 137.904 | 137.904 | 137.904 | ms |
| memory 100 MiB — layout | 6 | 0.245 | 18.343 | 18.343 | 18.343 | ms |
| memory 100 MiB — startup | 2 | 382.937 | 388.990 | 388.990 | 388.990 | ms |
| memory 100 MiB — file_open | 2 | 229.632 | 232.137 | 232.137 | 232.137 | ms |
| memory 100 MiB — memory_idle_30s | 2 | 1298186240.000 | 1366327296.000 | 1366327296.000 | 1366327296.000 | bytes |
| memory 100 MiB — memory_load | 2 | 350437376.000 | 350552064.000 | 350552064.000 | 350552064.000 | bytes |
| memory 100 MiB — memory_visible_layout | 2 | 384565248.000 | 384843776.000 | 384843776.000 | 384843776.000 | bytes |
| memory 10k work folder — frame_interval | 8 | 126.105 | 535.324 | 535.324 | 535.324 | ms |
| memory 10k work folder — layout | 10 | 14.144 | 38.363 | 38.363 | 38.363 | ms |
| memory 10k work folder — startup | 2 | 189.650 | 200.120 | 200.120 | 200.120 | ms |
| memory 10k work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory 10k work folder — memory_visible_layout | 2 | 71483392.000 | 71516160.000 | 71516160.000 | 71516160.000 | bytes |
| memory 10k work folder — memory_work_folder_idle | 2 | 405569536.000 | 406618112.000 | 406618112.000 | 406618112.000 | bytes |
| memory 1k work folder — keystroke_to_model | 2 | 0.001 | 0.001 | 0.001 | 0.001 | ms |
| memory 1k work folder — keystroke_to_frame | 2 | 14.347 | 61.901 | 61.901 | 61.901 | ms |
| memory 1k work folder — frame_interval | 13 | 40.905 | 219.250 | 219.250 | 219.250 | ms |
| memory 1k work folder — layout | 15 | 1.433 | 10.578 | 10.578 | 10.578 | ms |
| memory 1k work folder — startup | 2 | 182.781 | 204.624 | 204.624 | 204.624 | ms |
| memory 1k work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory 1k work folder — memory_visible_layout | 2 | 71450624.000 | 71581696.000 | 71581696.000 | 71581696.000 | bytes |
| memory 1k work folder — memory_work_folder_idle | 2 | 124141568.000 | 126615552.000 | 126615552.000 | 126615552.000 | bytes |
| memory after large then small — frame_interval | 10 | 16.864 | 140.543 | 140.543 | 140.543 | ms |
| memory after large then small — layout | 11 | 0.104 | 1.950 | 1.950 | 1.950 | ms |
| memory after large then small — startup | 1 | 196.324 | 196.324 | 196.324 | 196.324 | ms |
| memory after large then small — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory after large then small — memory_after_2_notes | 1 | 355090432.000 | 355090432.000 | 355090432.000 | 355090432.000 | bytes |
| memory after large then small — memory_visible_layout | 1 | 71581696.000 | 71581696.000 | 71581696.000 | 71581696.000 | bytes |
| memory after visiting notes — frame_interval | 2170 | 31.345 | 45.514 | 48.403 | 31362.511 | ms |
| memory after visiting notes — layout | 2171 | 2.437 | 3.715 | 3.945 | 6.121 | ms |
| memory after visiting notes — startup | 1 | 184.493 | 184.493 | 184.493 | 184.493 | ms |
| memory after visiting notes — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory after visiting notes — memory_after_1000_notes | 1 | 258473984.000 | 258473984.000 | 258473984.000 | 258473984.000 | bytes |
| memory after visiting notes — memory_after_100_notes | 1 | 143245312.000 | 143245312.000 | 143245312.000 | 143245312.000 | bytes |
| memory after visiting notes — memory_after_10_notes | 1 | 133529600.000 | 133529600.000 | 133529600.000 | 133529600.000 | bytes |
| memory after visiting notes — memory_visible_layout | 1 | 71532544.000 | 71532544.000 | 71532544.000 | 71532544.000 | bytes |
| memory empty editor — frame_interval | 10 | 44.931 | 1838.525 | 1838.525 | 1838.525 | ms |
| memory empty editor — layout | 12 | 0.204 | 6.283 | 6.283 | 6.283 | ms |
| memory empty editor — startup | 2 | 189.347 | 256.663 | 256.663 | 256.663 | ms |
| memory empty editor — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory empty editor — memory_idle_30s | 2 | 97533952.000 | 98467840.000 | 98467840.000 | 98467840.000 | bytes |
| memory empty editor — memory_visible_layout | 2 | 71647232.000 | 71794688.000 | 71794688.000 | 71794688.000 | bytes |
| memory empty work folder — frame_interval | 4 | 20.758 | 125.312 | 125.312 | 125.312 | ms |
| memory empty work folder — layout | 6 | 0.104 | 0.145 | 0.145 | 0.145 | ms |
| memory empty work folder — startup | 2 | 177.572 | 194.956 | 194.956 | 194.956 | ms |
| memory empty work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory empty work folder — memory_visible_layout | 2 | 71581696.000 | 71598080.000 | 71598080.000 | 71598080.000 | bytes |
| memory empty work folder — memory_work_folder_idle | 2 | 76595200.000 | 87818240.000 | 87818240.000 | 87818240.000 | bytes |
| memory long-running switches, edits, and images — keystroke_to_model | 200 | 0.000 | 0.002 | 0.016 | 0.026 | ms |
| memory long-running switches, edits, and images — keystroke_to_frame | 200 | 2.218 | 5.021 | 5.031 | 5.061 | ms |
| memory long-running switches, edits, and images — frame_interval | 79 | 43.815 | 990.560 | 1000.233 | 1000.233 | ms |
| memory long-running switches, edits, and images — layout | 80 | 0.877 | 1.734 | 1.922 | 1.922 | ms |
| memory long-running switches, edits, and images — startup | 1 | 189.097 | 189.097 | 189.097 | 189.097 | ms |
| memory long-running switches, edits, and images — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory long-running switches, edits, and images — block_index_update | 40 | 0.007 | 0.009 | 0.017 | 0.017 | ms |
| memory long-running switches, edits, and images — block_index_reparsed_bytes | 40 | 1024.000 | 1025.000 | 1025.000 | 1025.000 | bytes |
| memory long-running switches, edits, and images — block_index_invalidated_blocks | 40 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| memory long-running switches, edits, and images — memory_longrun_cycle_1 | 1 | 99106816.000 | 99106816.000 | 99106816.000 | 99106816.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_10 | 1 | 109723648.000 | 109723648.000 | 109723648.000 | 109723648.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_15 | 1 | 109805568.000 | 109805568.000 | 109805568.000 | 109805568.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_20 | 1 | 109871104.000 | 109871104.000 | 109871104.000 | 109871104.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_5 | 1 | 109674496.000 | 109674496.000 | 109674496.000 | 109674496.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_idle | 1 | 109166592.000 | 109166592.000 | 109166592.000 | 109166592.000 | bytes |
| memory long-running switches, edits, and images — memory_visible_layout | 1 | 71647232.000 | 71647232.000 | 71647232.000 | 71647232.000 | bytes |
| small document warm startup — frame_interval | 55 | 15.742 | 18.866 | 26.805 | 26.805 | ms |
| small document warm startup — layout | 115 | 2.021 | 2.315 | 2.391 | 2.407 | ms |
| small document warm startup — startup | 60 | 196.986 | 227.932 | 238.674 | 238.674 | ms |
| small document warm startup — file_open | 60 | 70.992 | 74.895 | 75.543 | 75.543 | ms |
| small document warm startup — memory_load | 60 | 68190208.000 | 68403200.000 | 70582272.000 | 70582272.000 | bytes |

## Work folder scan completion

| Scenario | Indexed Markdown files | Samples | Median | p95 | Max | Unit |
|---|---:|---:|---:|---:|---:|---|
| memory_folder_10k | 10000 | 2 | 314.473 | 733.429 | 733.429 | ms |
| memory_folder_1k | 1000 | 2 | 340.980 | 433.725 | 433.725 | ms |
| memory_folder_empty | 0 | 2 | 309.913 | 326.772 | 326.772 | ms |
| memory_large_then_small | 2 | 1 | 317.382 | 317.382 | 317.382 | ms |
| memory_longrun | 2 | 1 | 270.433 | 270.433 | 270.433 | ms |
| memory_visits_1k | 1000 | 1 | 326.285 | 326.285 | 326.285 | ms |
| startup_folder_10k | 10000 | 60 | 646.099 | 681.726 | 698.175 | ms |
| startup_folder_1k | 1000 | 60 | 312.885 | 340.808 | 364.881 | ms |
| startup_folder_empty | 0 | 60 | 307.835 | 342.096 | 356.369 | ms |
