# UI measurement results

- Git: `5576d8e9692fb629fc242170ccaab73eb1314b5c`
- Profile: `release`
- Rust: `rustc 1.98.1 (48a229cea 2026-09-01)`
- GPUI: `gpui-pre 0.3.6`
- OS: `26.6.2`
- CPU: `Apple M3 Pro`
- Input sources: `com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese, com.apple.keylayout.ABC`
- Refresh rate: `variable (CGDisplayMode reports 0)` Hz
- Background job states: `false, true`

| Scenario / metric | Samples | Median | p95 | p99 | Max | Unit |
|---|---:|---:|---:|---:|---:|---|
| 100 MB input at end — keystroke_to_model | 30 | 0.006 | 0.009 | 0.016 | 0.016 | ms |
| 100 MB input at end — keystroke_to_frame | 30 | 4.110 | 4.404 | 4.429 | 4.429 | ms |
| 100 MB input at end — frame_interval | 31 | 24.994 | 417.187 | 513.138 | 513.138 | ms |
| 100 MB input at end — layout | 31 | 0.427 | 0.768 | 0.916 | 0.916 | ms |
| 100 MB input at end — startup | 1 | 369.785 | 369.785 | 369.785 | 369.785 | ms |
| 100 MB input at end — file_open | 1 | 238.856 | 238.856 | 238.856 | 238.856 | ms |
| 100 MB input at end — block_index_update | 2 | 0.004 | 0.006 | 0.006 | 0.006 | ms |
| 100 MB input at end — block_index_reparsed_bytes | 2 | 125.000 | 126.000 | 126.000 | 126.000 | bytes |
| 100 MB input at end — block_index_invalidated_blocks | 2 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100 MB input at end — memory_load | 1 | 349585408.000 | 349585408.000 | 349585408.000 | 349585408.000 | bytes |
| 100 MB input at middle — keystroke_to_model | 30 | 0.006 | 0.011 | 0.013 | 0.013 | ms |
| 100 MB input at middle — keystroke_to_frame | 30 | 4.454 | 4.796 | 11.679 | 11.679 | ms |
| 100 MB input at middle — frame_interval | 31 | 24.985 | 461.248 | 566.746 | 566.746 | ms |
| 100 MB input at middle — layout | 31 | 0.715 | 0.893 | 8.128 | 8.128 | ms |
| 100 MB input at middle — startup | 1 | 373.888 | 373.888 | 373.888 | 373.888 | ms |
| 100 MB input at middle — file_open | 1 | 235.680 | 235.680 | 235.680 | 235.680 | ms |
| 100 MB input at middle — memory_load | 1 | 349634560.000 | 349634560.000 | 349634560.000 | 349634560.000 | bytes |
| 100 MB input at start — keystroke_to_model | 30 | 0.005 | 0.007 | 0.008 | 0.008 | ms |
| 100 MB input at start — keystroke_to_frame | 30 | 3.705 | 4.184 | 4.262 | 4.262 | ms |
| 100 MB input at start — frame_interval | 31 | 24.986 | 420.263 | 658.746 | 658.746 | ms |
| 100 MB input at start — layout | 31 | 0.295 | 0.354 | 0.365 | 0.365 | ms |
| 100 MB input at start — startup | 1 | 381.613 | 381.613 | 381.613 | 381.613 | ms |
| 100 MB input at start — file_open | 1 | 246.441 | 246.441 | 246.441 | 246.441 | ms |
| 100 MB input at start — memory_load | 1 | 349732864.000 | 349732864.000 | 349732864.000 | 349732864.000 | bytes |
| 100 MB input combined — keystroke_to_model | 90 | 0.006 | 0.009 | 0.016 | 0.016 | ms |
| 100 MB input combined — keystroke_to_frame | 90 | 4.133 | 4.634 | 11.679 | 11.679 | ms |
| 100 MB input combined — layout | 93 | 0.425 | 0.816 | 8.128 | 8.128 | ms |
| 100 MB input while scrolling — keystroke_to_model | 30 | 0.005 | 0.008 | 0.009 | 0.009 | ms |
| 100 MB input while scrolling — keystroke_to_frame | 30 | 3.124 | 4.681 | 4.878 | 4.878 | ms |
| 100 MB input while scrolling — frame_interval | 189 | 8.302 | 9.005 | 21.410 | 406.731 | ms |
| 100 MB input while scrolling — layout | 189 | 0.171 | 0.784 | 0.871 | 0.904 | ms |
| 100 MB input while scrolling — startup | 1 | 365.529 | 365.529 | 365.529 | 365.529 | ms |
| 100 MB input while scrolling — file_open | 1 | 234.288 | 234.288 | 234.288 | 234.288 | ms |
| 100 MB input while scrolling — block_index_update | 1 | 0.007 | 0.007 | 0.007 | 0.007 | ms |
| 100 MB input while scrolling — block_index_reparsed_bytes | 1 | 114.000 | 114.000 | 114.000 | 114.000 | bytes |
| 100 MB input while scrolling — block_index_invalidated_blocks | 1 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100 MB input while scrolling — memory_load | 1 | 349601792.000 | 349601792.000 | 349601792.000 | 349601792.000 | bytes |
| 100 MB scroll only — frame_interval | 187 | 8.332 | 8.692 | 17.899 | 21.818 | ms |
| 100 MB scroll only — layout | 187 | 0.145 | 0.226 | 0.272 | 0.891 | ms |
| 100 MB scroll only — startup | 1 | 365.144 | 365.144 | 365.144 | 365.144 | ms |
| 100 MB scroll only — file_open | 1 | 233.242 | 233.242 | 233.242 | 233.242 | ms |
| 100 MB scroll only — memory_load | 1 | 349716480.000 | 349716480.000 | 349716480.000 | 349716480.000 | bytes |
| 100 MiB document warm startup — frame_interval | 27 | 16.601 | 20.495 | 59.207 | 59.207 | ms |
| 100 MiB document warm startup — layout | 57 | 17.141 | 19.759 | 22.456 | 22.456 | ms |
| 100 MiB document warm startup — startup | 30 | 385.340 | 418.998 | 442.807 | 442.807 | ms |
| 100 MiB document warm startup — file_open | 30 | 239.404 | 252.134 | 286.292 | 286.292 | ms |
| 100 MiB document warm startup — memory_load | 30 | 349634560.000 | 351485952.000 | 351698944.000 | 351698944.000 | bytes |
| 100k paragraphs input at end — keystroke_to_model | 33 | 0.003 | 0.009 | 0.009 | 0.009 | ms |
| 100k paragraphs input at end — keystroke_to_frame | 33 | 29.714 | 35.844 | 38.225 | 38.225 | ms |
| 100k paragraphs input at end — frame_interval | 41 | 30.095 | 43.483 | 327.652 | 327.652 | ms |
| 100k paragraphs input at end — layout | 41 | 0.233 | 0.334 | 0.548 | 0.548 | ms |
| 100k paragraphs input at end — startup | 1 | 202.729 | 202.729 | 202.729 | 202.729 | ms |
| 100k paragraphs input at end — file_open | 1 | 85.709 | 85.709 | 85.709 | 85.709 | ms |
| 100k paragraphs input at end — block_index_update | 33 | 27.479 | 33.493 | 35.440 | 35.440 | ms |
| 100k paragraphs input at end — block_index_reparsed_bytes | 33 | 3100019.000 | 3100034.000 | 3100035.000 | 3100035.000 | bytes |
| 100k paragraphs input at end — block_index_invalidated_blocks | 33 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100k paragraphs input at end — memory_load | 1 | 74989568.000 | 74989568.000 | 74989568.000 | 74989568.000 | bytes |
| 100k paragraphs input at middle — keystroke_to_model | 32 | 0.004 | 0.008 | 0.009 | 0.009 | ms |
| 100k paragraphs input at middle — keystroke_to_frame | 32 | 29.253 | 38.085 | 38.806 | 38.806 | ms |
| 100k paragraphs input at middle — frame_interval | 42 | 29.822 | 49.285 | 325.607 | 325.607 | ms |
| 100k paragraphs input at middle — layout | 42 | 0.333 | 0.498 | 0.525 | 0.525 | ms |
| 100k paragraphs input at middle — startup | 1 | 200.698 | 200.698 | 200.698 | 200.698 | ms |
| 100k paragraphs input at middle — file_open | 1 | 83.748 | 83.748 | 83.748 | 83.748 | ms |
| 100k paragraphs input at middle — block_index_update | 32 | 26.430 | 35.117 | 35.864 | 35.864 | ms |
| 100k paragraphs input at middle — block_index_reparsed_bytes | 32 | 3100019.000 | 3100034.000 | 3100035.000 | 3100035.000 | bytes |
| 100k paragraphs input at middle — block_index_invalidated_blocks | 32 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100k paragraphs input at middle — memory_load | 1 | 75071488.000 | 75071488.000 | 75071488.000 | 75071488.000 | bytes |
| 100k paragraphs input at start — keystroke_to_model | 32 | 0.003 | 0.007 | 0.009 | 0.009 | ms |
| 100k paragraphs input at start — keystroke_to_frame | 32 | 28.906 | 35.007 | 41.066 | 41.066 | ms |
| 100k paragraphs input at start — frame_interval | 41 | 30.107 | 48.915 | 321.963 | 321.963 | ms |
| 100k paragraphs input at start — layout | 41 | 0.298 | 0.425 | 0.453 | 0.453 | ms |
| 100k paragraphs input at start — startup | 1 | 199.393 | 199.393 | 199.393 | 199.393 | ms |
| 100k paragraphs input at start — file_open | 1 | 82.088 | 82.088 | 82.088 | 82.088 | ms |
| 100k paragraphs input at start — block_index_update | 32 | 26.611 | 32.727 | 38.082 | 38.082 | ms |
| 100k paragraphs input at start — block_index_reparsed_bytes | 32 | 3100019.000 | 3100034.000 | 3100035.000 | 3100035.000 | bytes |
| 100k paragraphs input at start — block_index_invalidated_blocks | 32 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100k paragraphs input at start — memory_load | 1 | 75104256.000 | 75104256.000 | 75104256.000 | 75104256.000 | bytes |
| 100k paragraphs input while scrolling — keystroke_to_model | 32 | 0.003 | 0.009 | 0.011 | 0.011 | ms |
| 100k paragraphs input while scrolling — keystroke_to_frame | 32 | 33.136 | 38.565 | 38.889 | 38.889 | ms |
| 100k paragraphs input while scrolling — frame_interval | 122 | 8.364 | 34.715 | 40.328 | 42.684 | ms |
| 100k paragraphs input while scrolling — layout | 122 | 0.146 | 0.403 | 0.543 | 0.666 | ms |
| 100k paragraphs input while scrolling — startup | 1 | 197.217 | 197.217 | 197.217 | 197.217 | ms |
| 100k paragraphs input while scrolling — file_open | 1 | 83.231 | 83.231 | 83.231 | 83.231 | ms |
| 100k paragraphs input while scrolling — block_index_update | 32 | 30.945 | 35.794 | 35.998 | 35.998 | ms |
| 100k paragraphs input while scrolling — block_index_reparsed_bytes | 32 | 3100019.000 | 3100034.000 | 3100035.000 | 3100035.000 | bytes |
| 100k paragraphs input while scrolling — block_index_invalidated_blocks | 32 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100k paragraphs input while scrolling — memory_load | 1 | 75137024.000 | 75137024.000 | 75137024.000 | 75137024.000 | bytes |
| 100k paragraphs scroll only — frame_interval | 189 | 8.333 | 9.065 | 9.644 | 9.763 | ms |
| 100k paragraphs scroll only — layout | 189 | 0.415 | 0.579 | 0.678 | 0.892 | ms |
| 100k paragraphs scroll only — startup | 1 | 204.090 | 204.090 | 204.090 | 204.090 | ms |
| 100k paragraphs scroll only — file_open | 1 | 81.867 | 81.867 | 81.867 | 81.867 | ms |
| 100k paragraphs scroll only — memory_load | 1 | 75055104.000 | 75055104.000 | 75055104.000 | 75055104.000 | bytes |
| 10k work folder warm startup — frame_interval | 105 | 58.423 | 393.145 | 423.302 | 435.236 | ms |
| 10k work folder warm startup — layout | 135 | 0.070 | 21.015 | 21.837 | 22.258 | ms |
| 10k work folder warm startup — startup | 30 | 186.819 | 203.377 | 219.522 | 219.522 | ms |
| 10k work folder warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| 1k work folder warm startup — frame_interval | 110 | 21.135 | 102.809 | 114.704 | 115.094 | ms |
| 1k work folder warm startup — layout | 140 | 0.127 | 2.841 | 2.960 | 3.866 | ms |
| 1k work folder warm startup — startup | 30 | 192.408 | 225.242 | 231.127 | 231.127 | ms |
| 1k work folder warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty cold startup (OS cache not purged) — frame_interval | 30 | 12.404 | 14.527 | 31.910 | 31.910 | ms |
| empty cold startup (OS cache not purged) — layout | 60 | 0.071 | 0.137 | 0.160 | 0.160 | ms |
| empty cold startup (OS cache not purged) — startup | 30 | 189.042 | 211.348 | 212.512 | 212.512 | ms |
| empty cold startup (OS cache not purged) — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty cold startup (OS cache not purged) — memory_ready | 30 | 70713344.000 | 70893568.000 | 70909952.000 | 70909952.000 | bytes |
| empty warm startup — frame_interval | 30 | 12.932 | 19.187 | 26.849 | 26.849 | ms |
| empty warm startup — layout | 60 | 0.082 | 0.137 | 0.405 | 0.405 | ms |
| empty warm startup — startup | 30 | 188.597 | 244.299 | 424.360 | 424.360 | ms |
| empty warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty warm startup — memory_ready | 30 | 70762496.000 | 70959104.000 | 70975488.000 | 70975488.000 | bytes |
| empty work folder warm startup — frame_interval | 139 | 12.340 | 75.033 | 87.573 | 92.395 | ms |
| empty work folder warm startup — layout | 169 | 0.070 | 0.129 | 0.146 | 0.157 | ms |
| empty work folder warm startup — startup | 30 | 192.423 | 224.635 | 227.108 | 227.108 | ms |
| empty work folder warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty work folder warm startup — memory_ready | 30 | 70696960.000 | 70893568.000 | 70942720.000 | 70942720.000 | bytes |
| input during background presentation update — keystroke_to_model | 30 | 0.006 | 0.008 | 0.008 | 0.008 | ms |
| input during background presentation update — keystroke_to_frame | 30 | 3.846 | 4.112 | 4.153 | 4.153 | ms |
| input during background presentation update — frame_interval | 66 | 12.613 | 71.410 | 294.000 | 294.000 | ms |
| input during background presentation update — layout | 66 | 0.141 | 0.323 | 0.361 | 0.361 | ms |
| input during background presentation update — startup | 1 | 189.990 | 189.990 | 189.990 | 189.990 | ms |
| input during background presentation update — file_open | 1 | 74.733 | 74.733 | 74.733 | 74.733 | ms |
| input during background presentation update — block_index_update | 30 | 2.355 | 2.669 | 2.776 | 2.776 | ms |
| input during background presentation update — block_index_reparsed_bytes | 30 | 1048596.000 | 1048610.000 | 1048611.000 | 1048611.000 | bytes |
| input during background presentation update — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| input during background presentation update — memory_load | 1 | 70008832.000 | 70008832.000 | 70008832.000 | 70008832.000 | bytes |
| memory 1 MiB — frame_interval | 6 | 29.437 | 2576.587 | 2576.587 | 2576.587 | ms |
| memory 1 MiB — layout | 8 | 0.300 | 6.016 | 6.016 | 6.016 | ms |
| memory 1 MiB — startup | 2 | 200.098 | 202.868 | 202.868 | 202.868 | ms |
| memory 1 MiB — file_open | 2 | 68.084 | 69.769 | 69.769 | 69.769 | ms |
| memory 1 MiB — memory_idle_30s | 2 | 121503744.000 | 122175488.000 | 122175488.000 | 122175488.000 | bytes |
| memory 1 MiB — memory_load | 2 | 70156288.000 | 70402048.000 | 70402048.000 | 70402048.000 | bytes |
| memory 1 MiB — memory_visible_layout | 2 | 82903040.000 | 83050496.000 | 83050496.000 | 83050496.000 | bytes |
| memory 10 MiB — frame_interval | 7 | 84.321 | 209.125 | 209.125 | 209.125 | ms |
| memory 10 MiB — layout | 9 | 0.269 | 8.096 | 8.096 | 8.096 | ms |
| memory 10 MiB — startup | 2 | 224.421 | 265.706 | 265.706 | 265.706 | ms |
| memory 10 MiB — file_open | 2 | 92.658 | 112.648 | 112.648 | 112.648 | ms |
| memory 10 MiB — memory_idle_30s | 2 | 296878080.000 | 370130944.000 | 370130944.000 | 370130944.000 | bytes |
| memory 10 MiB — memory_load | 2 | 95076352.000 | 95469568.000 | 95469568.000 | 95469568.000 | bytes |
| memory 10 MiB — memory_visible_layout | 2 | 109559808.000 | 110231552.000 | 110231552.000 | 110231552.000 | bytes |
| memory 100 MiB — frame_interval | 378 | 8.339 | 750.486 | 2574.737 | 11017.162 | ms |
| memory 100 MiB — layout | 380 | 0.343 | 1.003 | 1.311 | 18.475 | ms |
| memory 100 MiB — startup | 2 | 364.354 | 376.044 | 376.044 | 376.044 | ms |
| memory 100 MiB — file_open | 2 | 227.849 | 242.047 | 242.047 | 242.047 | ms |
| memory 100 MiB — memory_idle_30s | 2 | 1449951232.000 | 2155495424.000 | 2155495424.000 | 2155495424.000 | bytes |
| memory 100 MiB — memory_load | 2 | 349683712.000 | 349716480.000 | 349716480.000 | 349716480.000 | bytes |
| memory 100 MiB — memory_visible_layout | 2 | 383959040.000 | 384106496.000 | 384106496.000 | 384106496.000 | bytes |
| memory 10k work folder — frame_interval | 15 | 115.203 | 8862.036 | 8862.036 | 8862.036 | ms |
| memory 10k work folder — layout | 17 | 13.217 | 26.483 | 26.483 | 26.483 | ms |
| memory 10k work folder — startup | 2 | 182.354 | 184.399 | 184.399 | 184.399 | ms |
| memory 10k work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory 10k work folder — memory_visible_layout | 2 | 70664192.000 | 70729728.000 | 70729728.000 | 70729728.000 | bytes |
| memory 10k work folder — memory_work_folder_idle | 2 | 418693120.000 | 477822976.000 | 477822976.000 | 477822976.000 | bytes |
| memory 1k work folder — frame_interval | 10 | 17.239 | 181.270 | 181.270 | 181.270 | ms |
| memory 1k work folder — layout | 12 | 1.374 | 3.241 | 3.241 | 3.241 | ms |
| memory 1k work folder — startup | 2 | 188.984 | 223.048 | 223.048 | 223.048 | ms |
| memory 1k work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory 1k work folder — memory_visible_layout | 2 | 70696960.000 | 70762496.000 | 70762496.000 | 70762496.000 | bytes |
| memory 1k work folder — memory_work_folder_idle | 2 | 123076608.000 | 123568128.000 | 123568128.000 | 123568128.000 | bytes |
| memory after large then small — frame_interval | 7 | 15.160 | 302.906 | 302.906 | 302.906 | ms |
| memory after large then small — layout | 8 | 0.154 | 1.864 | 1.864 | 1.864 | ms |
| memory after large then small — startup | 1 | 184.140 | 184.140 | 184.140 | 184.140 | ms |
| memory after large then small — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory after large then small — memory_after_2_notes | 1 | 354680832.000 | 354680832.000 | 354680832.000 | 354680832.000 | bytes |
| memory after large then small — memory_visible_layout | 1 | 70828032.000 | 70828032.000 | 70828032.000 | 70828032.000 | bytes |
| memory after visiting notes — frame_interval | 995 | 20.268 | 40.345 | 43.774 | 31372.858 | ms |
| memory after visiting notes — layout | 996 | 1.782 | 2.541 | 2.645 | 4.829 | ms |
| memory after visiting notes — startup | 1 | 185.926 | 185.926 | 185.926 | 185.926 | ms |
| memory after visiting notes — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory after visiting notes — memory_after_1000_notes | 1 | 188628992.000 | 188628992.000 | 188628992.000 | 188628992.000 | bytes |
| memory after visiting notes — memory_after_100_notes | 1 | 144015360.000 | 144015360.000 | 144015360.000 | 144015360.000 | bytes |
| memory after visiting notes — memory_after_10_notes | 1 | 132546560.000 | 132546560.000 | 132546560.000 | 132546560.000 | bytes |
| memory after visiting notes — memory_visible_layout | 1 | 70729728.000 | 70729728.000 | 70729728.000 | 70729728.000 | bytes |
| memory empty editor — frame_interval | 12 | 30.615 | 1698.597 | 1698.597 | 1698.597 | ms |
| memory empty editor — layout | 14 | 0.126 | 5.829 | 5.829 | 5.829 | ms |
| memory empty editor — startup | 2 | 184.325 | 185.371 | 185.371 | 185.371 | ms |
| memory empty editor — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory empty editor — memory_idle_30s | 2 | 96993280.000 | 97796096.000 | 97796096.000 | 97796096.000 | bytes |
| memory empty editor — memory_visible_layout | 2 | 70844416.000 | 70959104.000 | 70959104.000 | 70959104.000 | bytes |
| memory empty work folder — frame_interval | 14 | 13.479 | 79.683 | 79.683 | 79.683 | ms |
| memory empty work folder — layout | 16 | 0.068 | 0.142 | 0.142 | 0.142 | ms |
| memory empty work folder — startup | 2 | 186.561 | 202.601 | 202.601 | 202.601 | ms |
| memory empty work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory empty work folder — memory_visible_layout | 2 | 70565888.000 | 70713344.000 | 70713344.000 | 70713344.000 | bytes |
| memory empty work folder — memory_work_folder_idle | 2 | 87212032.000 | 87425024.000 | 87425024.000 | 87425024.000 | bytes |
| memory long-running switches, edits, and images — keystroke_to_model | 200 | 0.000 | 0.002 | 0.016 | 0.019 | ms |
| memory long-running switches, edits, and images — keystroke_to_frame | 200 | 7.454 | 8.547 | 8.551 | 8.568 | ms |
| memory long-running switches, edits, and images — frame_interval | 75 | 46.335 | 936.315 | 989.440 | 989.440 | ms |
| memory long-running switches, edits, and images — layout | 76 | 0.858 | 1.778 | 1.897 | 1.897 | ms |
| memory long-running switches, edits, and images — startup | 1 | 175.583 | 175.583 | 175.583 | 175.583 | ms |
| memory long-running switches, edits, and images — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory long-running switches, edits, and images — block_index_update | 80 | 0.006 | 0.009 | 0.024 | 0.024 | ms |
| memory long-running switches, edits, and images — block_index_reparsed_bytes | 80 | 1024.000 | 1025.000 | 1025.000 | 1025.000 | bytes |
| memory long-running switches, edits, and images — block_index_invalidated_blocks | 80 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| memory long-running switches, edits, and images — memory_longrun_cycle_1 | 1 | 97615872.000 | 97615872.000 | 97615872.000 | 97615872.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_10 | 1 | 108314624.000 | 108314624.000 | 108314624.000 | 108314624.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_15 | 1 | 108412928.000 | 108412928.000 | 108412928.000 | 108412928.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_20 | 1 | 108822528.000 | 108822528.000 | 108822528.000 | 108822528.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_5 | 1 | 108134400.000 | 108134400.000 | 108134400.000 | 108134400.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_idle | 1 | 108167168.000 | 108167168.000 | 108167168.000 | 108167168.000 | bytes |
| memory long-running switches, edits, and images — memory_visible_layout | 1 | 70844416.000 | 70844416.000 | 70844416.000 | 70844416.000 | bytes |
| normal ASCII input — keystroke_to_model | 30 | 0.012 | 0.019 | 0.021 | 0.021 | ms |
| normal ASCII input — keystroke_to_frame | 30 | 4.768 | 5.447 | 5.695 | 5.695 | ms |
| normal ASCII input — frame_interval | 64 | 11.220 | 22.079 | 449.387 | 449.387 | ms |
| normal ASCII input — layout | 64 | 0.277 | 0.531 | 0.575 | 0.575 | ms |
| normal ASCII input — startup | 1 | 255.105 | 255.105 | 255.105 | 255.105 | ms |
| normal ASCII input — file_open | 1 | 81.359 | 81.359 | 81.359 | 81.359 | ms |
| normal ASCII input — block_index_update | 30 | 3.379 | 4.026 | 4.033 | 4.033 | ms |
| normal ASCII input — block_index_reparsed_bytes | 30 | 1048596.000 | 1048610.000 | 1048611.000 | 1048611.000 | bytes |
| normal ASCII input — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| normal ASCII input — memory_load | 1 | 69877760.000 | 69877760.000 | 69877760.000 | 69877760.000 | bytes |
| real Japanese IME composition to commit — keystroke_to_model | 240 | 0.004 | 0.007 | 0.008 | 0.020 | ms |
| real Japanese IME composition to commit — keystroke_to_frame | 240 | 4.052 | 5.474 | 8.088 | 8.358 | ms |
| real Japanese IME composition to commit — frame_interval | 287 | 10.959 | 20.504 | 39.884 | 481.336 | ms |
| real Japanese IME composition to commit — layout | 287 | 0.236 | 0.387 | 0.467 | 0.481 | ms |
| real Japanese IME composition to commit — startup | 1 | 191.180 | 191.180 | 191.180 | 191.180 | ms |
| real Japanese IME composition to commit — file_open | 1 | 78.384 | 78.384 | 78.384 | 78.384 | ms |
| real Japanese IME composition to commit — block_index_update | 240 | 3.048 | 3.340 | 4.490 | 6.306 | ms |
| real Japanese IME composition to commit — block_index_reparsed_bytes | 240 | 1048759.000 | 1048879.000 | 1048891.000 | 1048894.000 | bytes |
| real Japanese IME composition to commit — block_index_invalidated_blocks | 240 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| real Japanese IME composition to commit — ime_commit_to_model | 30 | 0.003 | 0.006 | 0.006 | 0.006 | ms |
| real Japanese IME composition to commit — ime_commit_to_frame | 30 | 3.783 | 4.237 | 4.415 | 4.415 | ms |
| real Japanese IME composition to commit — memory_load | 1 | 69992448.000 | 69992448.000 | 69992448.000 | 69992448.000 | bytes |
| small document warm startup — frame_interval | 28 | 15.285 | 21.628 | 29.355 | 29.355 | ms |
| small document warm startup — layout | 58 | 1.001 | 2.324 | 2.346 | 2.346 | ms |
| small document warm startup — startup | 30 | 200.667 | 222.585 | 229.343 | 229.343 | ms |
| small document warm startup — file_open | 30 | 71.150 | 75.689 | 79.352 | 79.352 | ms |
| small document warm startup — memory_load | 30 | 67452928.000 | 69533696.000 | 69533696.000 | 69533696.000 | bytes |

## Work folder scan completion

| Scenario | Indexed Markdown files | Samples | Median | p95 | Max | Unit |
|---|---:|---:|---:|---:|---:|---|
| memory_folder_10k | 10000 | 2 | 628.441 | 642.188 | 642.188 | ms |
| memory_folder_1k | 1000 | 2 | 336.279 | 426.399 | 426.399 | ms |
| memory_folder_empty | 0 | 2 | 278.297 | 339.910 | 339.910 | ms |
| memory_large_then_small | 2 | 1 | 306.595 | 306.595 | 306.595 | ms |
| memory_longrun | 2 | 1 | 302.369 | 302.369 | 302.369 | ms |
| memory_visits_1k | 1000 | 1 | 299.484 | 299.484 | 299.484 | ms |
| startup_folder_10k | 10000 | 30 | 659.870 | 692.250 | 703.493 | ms |
| startup_folder_1k | 1000 | 30 | 322.066 | 362.686 | 365.963 | ms |
| startup_folder_empty | 0 | 30 | 313.440 | 361.887 | 372.737 | ms |
