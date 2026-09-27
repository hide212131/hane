# UI measurement results

- Git: `feedda47104c1fe8cb0d454ce329dd8d043a24bd`
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
| 100 MB input at end — keystroke_to_model | 30 | 0.008 | 0.029 | 0.035 | 0.035 | ms |
| 100 MB input at end — keystroke_to_frame | 30 | 2.243 | 3.577 | 3.970 | 3.970 | ms |
| 100 MB input at end — frame_interval | 31 | 25.149 | 33.607 | 1659.841 | 1659.841 | ms |
| 100 MB input at end — layout | 31 | 0.327 | 0.653 | 0.862 | 0.862 | ms |
| 100 MB input at end — startup | 2 | 357.152 | 371.253 | 371.253 | 371.253 | ms |
| 100 MB input at end — file_open | 2 | 226.805 | 228.435 | 228.435 | 228.435 | ms |
| 100 MB input at end — block_index_update | 30 | 0.014 | 0.074 | 0.102 | 0.102 | ms |
| 100 MB input at end — block_index_reparsed_bytes | 30 | 111.000 | 125.000 | 126.000 | 126.000 | bytes |
| 100 MB input at end — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100 MB input at end — memory_load | 2 | 349880320.000 | 350027776.000 | 350027776.000 | 350027776.000 | bytes |
| 100 MB input at middle — keystroke_to_model | 30 | 0.011 | 0.026 | 0.039 | 0.039 | ms |
| 100 MB input at middle — keystroke_to_frame | 30 | 2.798 | 5.495 | 5.621 | 5.621 | ms |
| 100 MB input at middle — frame_interval | 31 | 24.172 | 34.982 | 1690.202 | 1690.202 | ms |
| 100 MB input at middle — layout | 31 | 0.661 | 1.007 | 1.024 | 1.024 | ms |
| 100 MB input at middle — startup | 2 | 347.857 | 366.144 | 366.144 | 366.144 | ms |
| 100 MB input at middle — file_open | 2 | 217.115 | 231.434 | 231.434 | 231.434 | ms |
| 100 MB input at middle — block_index_update | 30 | 0.023 | 0.054 | 0.063 | 0.063 | ms |
| 100 MB input at middle — block_index_reparsed_bytes | 30 | 118.000 | 132.000 | 133.000 | 133.000 | bytes |
| 100 MB input at middle — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100 MB input at middle — memory_load | 2 | 349896704.000 | 349929472.000 | 349929472.000 | 349929472.000 | bytes |
| 100 MB input at start — keystroke_to_model | 30 | 0.003 | 0.022 | 0.043 | 0.043 | ms |
| 100 MB input at start — keystroke_to_frame | 30 | 0.754 | 4.171 | 4.191 | 4.191 | ms |
| 100 MB input at start — frame_interval | 31 | 2.792 | 403.570 | 1522.798 | 1522.798 | ms |
| 100 MB input at start — layout | 31 | 0.182 | 0.427 | 0.431 | 0.431 | ms |
| 100 MB input at start — startup | 2 | 343.618 | 354.459 | 354.459 | 354.459 | ms |
| 100 MB input at start — file_open | 2 | 222.791 | 231.188 | 231.188 | 231.188 | ms |
| 100 MB input at start — block_index_update | 29 | 0.007 | 0.064 | 0.064 | 0.064 | ms |
| 100 MB input at start — block_index_reparsed_bytes | 29 | 100.000 | 113.000 | 114.000 | 114.000 | bytes |
| 100 MB input at start — block_index_invalidated_blocks | 29 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100 MB input at start — memory_load | 2 | 349470720.000 | 349831168.000 | 349831168.000 | 349831168.000 | bytes |
| 100 MB input combined — keystroke_to_model | 90 | 0.008 | 0.027 | 0.043 | 0.043 | ms |
| 100 MB input combined — keystroke_to_frame | 90 | 2.450 | 4.648 | 5.621 | 5.621 | ms |
| 100 MB input combined — layout | 93 | 0.310 | 0.927 | 1.024 | 1.024 | ms |
| 100 MB input while scrolling — keystroke_to_model | 30 | 0.009 | 0.022 | 0.022 | 0.022 | ms |
| 100 MB input while scrolling — keystroke_to_frame | 30 | 2.952 | 3.858 | 4.148 | 4.148 | ms |
| 100 MB input while scrolling — frame_interval | 192 | 8.305 | 9.126 | 23.326 | 407.516 | ms |
| 100 MB input while scrolling — layout | 192 | 0.176 | 0.498 | 0.582 | 0.761 | ms |
| 100 MB input while scrolling — startup | 2 | 345.869 | 361.338 | 361.338 | 361.338 | ms |
| 100 MB input while scrolling — file_open | 2 | 219.487 | 232.242 | 232.242 | 232.242 | ms |
| 100 MB input while scrolling — block_index_update | 30 | 0.021 | 0.034 | 0.043 | 0.043 | ms |
| 100 MB input while scrolling — block_index_reparsed_bytes | 30 | 99.000 | 113.000 | 114.000 | 114.000 | bytes |
| 100 MB input while scrolling — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100 MB input while scrolling — memory_load | 2 | 349913088.000 | 349945856.000 | 349945856.000 | 349945856.000 | bytes |
| 100 MB scroll only — frame_interval | 185 | 8.331 | 9.430 | 13.345 | 16.703 | ms |
| 100 MB scroll only — layout | 185 | 0.408 | 0.580 | 0.693 | 0.899 | ms |
| 100 MB scroll only — startup | 2 | 346.437 | 355.468 | 355.468 | 355.468 | ms |
| 100 MB scroll only — file_open | 2 | 222.611 | 230.622 | 230.622 | 230.622 | ms |
| 100 MB scroll only — memory_load | 2 | 349945856.000 | 350093312.000 | 350093312.000 | 350093312.000 | bytes |
| 100 MiB document warm startup — frame_interval | 27 | 12.761 | 13.324 | 13.678 | 13.678 | ms |
| 100 MiB document warm startup — layout | 57 | 16.263 | 17.574 | 18.343 | 18.343 | ms |
| 100 MiB document warm startup — startup | 30 | 343.596 | 367.273 | 367.319 | 367.319 | ms |
| 100 MiB document warm startup — file_open | 30 | 218.981 | 232.785 | 235.550 | 235.550 | ms |
| 100 MiB document warm startup — memory_load | 30 | 350027776.000 | 350142464.000 | 350142464.000 | 350142464.000 | bytes |
| 100k paragraphs input at end — keystroke_to_model | 30 | 0.003 | 0.008 | 0.013 | 0.013 | ms |
| 100k paragraphs input at end — keystroke_to_frame | 30 | 33.300 | 35.606 | 44.017 | 44.017 | ms |
| 100k paragraphs input at end — frame_interval | 33 | 33.703 | 46.940 | 1403.267 | 1403.267 | ms |
| 100k paragraphs input at end — layout | 33 | 0.226 | 0.348 | 0.396 | 0.396 | ms |
| 100k paragraphs input at end — startup | 2 | 175.050 | 190.265 | 190.265 | 190.265 | ms |
| 100k paragraphs input at end — file_open | 2 | 66.356 | 76.056 | 76.056 | 76.056 | ms |
| 100k paragraphs input at end — block_index_update | 30 | 31.362 | 33.657 | 41.402 | 41.402 | ms |
| 100k paragraphs input at end — block_index_reparsed_bytes | 30 | 3100020.000 | 3100034.000 | 3100035.000 | 3100035.000 | bytes |
| 100k paragraphs input at end — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100k paragraphs input at end — memory_load | 2 | 75300864.000 | 75431936.000 | 75431936.000 | 75431936.000 | bytes |
| 100k paragraphs input at middle — keystroke_to_model | 30 | 0.003 | 0.006 | 0.007 | 0.007 | ms |
| 100k paragraphs input at middle — keystroke_to_frame | 30 | 33.296 | 38.075 | 38.602 | 38.602 | ms |
| 100k paragraphs input at middle — frame_interval | 35 | 33.803 | 48.051 | 1378.748 | 1378.748 | ms |
| 100k paragraphs input at middle — layout | 35 | 0.320 | 0.434 | 0.445 | 0.445 | ms |
| 100k paragraphs input at middle — startup | 2 | 174.836 | 180.936 | 180.936 | 180.936 | ms |
| 100k paragraphs input at middle — file_open | 2 | 67.619 | 72.143 | 72.143 | 72.143 | ms |
| 100k paragraphs input at middle — block_index_update | 30 | 31.183 | 35.969 | 36.324 | 36.324 | ms |
| 100k paragraphs input at middle — block_index_reparsed_bytes | 30 | 3100020.000 | 3100034.000 | 3100035.000 | 3100035.000 | bytes |
| 100k paragraphs input at middle — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100k paragraphs input at middle — memory_load | 2 | 75350016.000 | 75382784.000 | 75382784.000 | 75382784.000 | bytes |
| 100k paragraphs input at start — keystroke_to_model | 30 | 0.003 | 0.007 | 0.008 | 0.008 | ms |
| 100k paragraphs input at start — keystroke_to_frame | 30 | 29.027 | 35.505 | 38.925 | 38.925 | ms |
| 100k paragraphs input at start — frame_interval | 34 | 29.525 | 45.338 | 1356.062 | 1356.062 | ms |
| 100k paragraphs input at start — layout | 34 | 0.285 | 0.444 | 0.566 | 0.566 | ms |
| 100k paragraphs input at start — startup | 2 | 186.567 | 194.452 | 194.452 | 194.452 | ms |
| 100k paragraphs input at start — file_open | 2 | 71.331 | 78.283 | 78.283 | 78.283 | ms |
| 100k paragraphs input at start — block_index_update | 30 | 26.758 | 33.354 | 36.206 | 36.206 | ms |
| 100k paragraphs input at start — block_index_reparsed_bytes | 30 | 3100020.000 | 3100034.000 | 3100035.000 | 3100035.000 | bytes |
| 100k paragraphs input at start — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100k paragraphs input at start — memory_load | 2 | 75300864.000 | 75513856.000 | 75513856.000 | 75513856.000 | bytes |
| 100k paragraphs input while scrolling — keystroke_to_model | 30 | 0.003 | 0.006 | 0.007 | 0.007 | ms |
| 100k paragraphs input while scrolling — keystroke_to_frame | 30 | 33.788 | 38.155 | 47.251 | 47.251 | ms |
| 100k paragraphs input while scrolling — frame_interval | 124 | 8.347 | 34.957 | 43.948 | 48.772 | ms |
| 100k paragraphs input while scrolling — layout | 124 | 0.156 | 0.391 | 0.495 | 0.527 | ms |
| 100k paragraphs input while scrolling — startup | 2 | 176.160 | 184.010 | 184.010 | 184.010 | ms |
| 100k paragraphs input while scrolling — file_open | 2 | 68.277 | 70.667 | 70.667 | 70.667 | ms |
| 100k paragraphs input while scrolling — block_index_update | 30 | 31.607 | 36.075 | 44.941 | 44.941 | ms |
| 100k paragraphs input while scrolling — block_index_reparsed_bytes | 30 | 3100020.000 | 3100034.000 | 3100035.000 | 3100035.000 | bytes |
| 100k paragraphs input while scrolling — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| 100k paragraphs input while scrolling — memory_load | 2 | 75251712.000 | 75268096.000 | 75268096.000 | 75268096.000 | bytes |
| 100k paragraphs scroll only — frame_interval | 194 | 8.325 | 9.157 | 9.709 | 10.007 | ms |
| 100k paragraphs scroll only — layout | 194 | 0.246 | 0.476 | 0.511 | 0.558 | ms |
| 100k paragraphs scroll only — startup | 2 | 174.242 | 193.946 | 193.946 | 193.946 | ms |
| 100k paragraphs scroll only — file_open | 2 | 66.114 | 76.267 | 76.267 | 76.267 | ms |
| 100k paragraphs scroll only — memory_load | 2 | 75251712.000 | 75431936.000 | 75431936.000 | 75431936.000 | bytes |
| 10k work folder warm startup — frame_interval | 30 | 8.752 | 9.949 | 10.058 | 10.058 | ms |
| 10k work folder warm startup — layout | 60 | 0.059 | 0.112 | 0.116 | 0.116 | ms |
| 10k work folder warm startup — startup | 30 | 163.414 | 173.232 | 190.496 | 190.496 | ms |
| 10k work folder warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| 1k work folder warm startup — frame_interval | 30 | 8.943 | 9.867 | 10.497 | 10.497 | ms |
| 1k work folder warm startup — layout | 60 | 0.067 | 0.115 | 0.135 | 0.135 | ms |
| 1k work folder warm startup — startup | 30 | 166.114 | 170.606 | 179.935 | 179.935 | ms |
| 1k work folder warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty cold startup (OS cache not purged) — frame_interval | 28 | 8.749 | 9.149 | 9.277 | 9.277 | ms |
| empty cold startup (OS cache not purged) — layout | 58 | 0.101 | 0.117 | 0.157 | 0.157 | ms |
| empty cold startup (OS cache not purged) — startup | 30 | 163.532 | 169.968 | 170.493 | 170.493 | ms |
| empty cold startup (OS cache not purged) — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty cold startup (OS cache not purged) — memory_ready | 30 | 70909952.000 | 71024640.000 | 71057408.000 | 71057408.000 | bytes |
| empty warm startup — frame_interval | 29 | 8.792 | 9.572 | 11.578 | 11.578 | ms |
| empty warm startup — layout | 59 | 0.102 | 0.115 | 0.363 | 0.363 | ms |
| empty warm startup — startup | 30 | 164.306 | 175.748 | 244.332 | 244.332 | ms |
| empty warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty warm startup — memory_ready | 30 | 70959104.000 | 71073792.000 | 71073792.000 | 71073792.000 | bytes |
| empty work folder warm startup — frame_interval | 30 | 8.788 | 10.343 | 10.377 | 10.377 | ms |
| empty work folder warm startup — layout | 60 | 0.062 | 0.116 | 0.122 | 0.122 | ms |
| empty work folder warm startup — startup | 30 | 164.454 | 175.837 | 178.456 | 178.456 | ms |
| empty work folder warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty work folder warm startup — memory_ready | 30 | 70991872.000 | 71139328.000 | 71172096.000 | 71172096.000 | bytes |
| input during background presentation update — keystroke_to_model | 30 | 0.005 | 0.008 | 0.008 | 0.008 | ms |
| input during background presentation update — keystroke_to_frame | 30 | 3.137 | 4.056 | 4.074 | 4.074 | ms |
| input during background presentation update — frame_interval | 64 | 11.075 | 21.612 | 450.914 | 450.914 | ms |
| input during background presentation update — layout | 64 | 0.144 | 0.313 | 0.354 | 0.354 | ms |
| input during background presentation update — startup | 2 | 169.147 | 191.443 | 191.443 | 191.443 | ms |
| input during background presentation update — file_open | 2 | 63.734 | 74.163 | 74.163 | 74.163 | ms |
| input during background presentation update — block_index_update | 30 | 2.325 | 2.577 | 2.644 | 2.644 | ms |
| input during background presentation update — block_index_reparsed_bytes | 30 | 1048596.000 | 1048610.000 | 1048611.000 | 1048611.000 | bytes |
| input during background presentation update — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| input during background presentation update — memory_load | 2 | 70041600.000 | 70107136.000 | 70107136.000 | 70107136.000 | bytes |
| memory 1 MiB — frame_interval | 2 | 12.298 | 13.442 | 13.442 | 13.442 | ms |
| memory 1 MiB — layout | 4 | 0.169 | 7.677 | 7.677 | 7.677 | ms |
| memory 1 MiB — startup | 2 | 199.834 | 201.971 | 201.971 | 201.971 | ms |
| memory 1 MiB — file_open | 2 | 65.891 | 69.015 | 69.015 | 69.015 | ms |
| memory 1 MiB — memory_idle_30s | 2 | 115048448.000 | 115146752.000 | 115146752.000 | 115146752.000 | bytes |
| memory 1 MiB — memory_load | 2 | 70270976.000 | 70287360.000 | 70287360.000 | 70287360.000 | bytes |
| memory 1 MiB — memory_visible_layout | 2 | 82952192.000 | 82968576.000 | 82968576.000 | 82968576.000 | bytes |
| memory 10 MiB — frame_interval | 2 | 11.375 | 13.163 | 13.163 | 13.163 | ms |
| memory 10 MiB — layout | 4 | 0.184 | 5.997 | 5.997 | 5.997 | ms |
| memory 10 MiB — startup | 2 | 207.531 | 231.652 | 231.652 | 231.652 | ms |
| memory 10 MiB — file_open | 2 | 86.032 | 91.437 | 91.437 | 91.437 | ms |
| memory 10 MiB — memory_idle_30s | 2 | 364019712.000 | 364134400.000 | 364134400.000 | 364134400.000 | bytes |
| memory 10 MiB — memory_load | 2 | 95404032.000 | 95567872.000 | 95567872.000 | 95567872.000 | bytes |
| memory 10 MiB — memory_visible_layout | 2 | 109953024.000 | 110133248.000 | 110133248.000 | 110133248.000 | bytes |
| memory 100 MiB — frame_interval | 2 | 13.005 | 13.038 | 13.038 | 13.038 | ms |
| memory 100 MiB — layout | 4 | 0.169 | 16.482 | 16.482 | 16.482 | ms |
| memory 100 MiB — startup | 2 | 356.212 | 360.799 | 360.799 | 360.799 | ms |
| memory 100 MiB — file_open | 2 | 213.158 | 213.526 | 213.526 | 213.526 | ms |
| memory 100 MiB — memory_idle_30s | 2 | 1292075008.000 | 1548550144.000 | 1548550144.000 | 1548550144.000 | bytes |
| memory 100 MiB — memory_load | 2 | 349913088.000 | 349913088.000 | 349913088.000 | 349913088.000 | bytes |
| memory 100 MiB — memory_visible_layout | 2 | 383713280.000 | 383713280.000 | 383713280.000 | 383713280.000 | bytes |
| memory 10k work folder — frame_interval | 2 | 9.362 | 9.663 | 9.663 | 9.663 | ms |
| memory 10k work folder — layout | 4 | 0.052 | 0.126 | 0.126 | 0.126 | ms |
| memory 10k work folder — startup | 2 | 181.335 | 198.890 | 198.890 | 198.890 | ms |
| memory 10k work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory 10k work folder — memory_visible_layout | 2 | 70909952.000 | 70942720.000 | 70942720.000 | 70942720.000 | bytes |
| memory 10k work folder — memory_work_folder_idle | 2 | 61931520.000 | 88670208.000 | 88670208.000 | 88670208.000 | bytes |
| memory 1k work folder — frame_interval | 2 | 8.462 | 10.011 | 10.011 | 10.011 | ms |
| memory 1k work folder — layout | 4 | 0.067 | 0.115 | 0.115 | 0.115 | ms |
| memory 1k work folder — startup | 2 | 165.788 | 180.919 | 180.919 | 180.919 | ms |
| memory 1k work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory 1k work folder — memory_visible_layout | 2 | 70877184.000 | 70959104.000 | 70959104.000 | 70959104.000 | bytes |
| memory 1k work folder — memory_work_folder_idle | 2 | 83935232.000 | 84279296.000 | 84279296.000 | 84279296.000 | bytes |
| memory after large then small — frame_interval | 10 | 13.135 | 210.593 | 210.593 | 210.593 | ms |
| memory after large then small — layout | 11 | 0.098 | 1.789 | 1.789 | 1.789 | ms |
| memory after large then small — startup | 1 | 184.731 | 184.731 | 184.731 | 184.731 | ms |
| memory after large then small — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory after large then small — memory_after_2_notes | 1 | 354254848.000 | 354254848.000 | 354254848.000 | 354254848.000 | bytes |
| memory after large then small — memory_visible_layout | 1 | 70877184.000 | 70877184.000 | 70877184.000 | 70877184.000 | bytes |
| memory after visiting notes — frame_interval | 2213 | 30.174 | 45.311 | 48.489 | 31374.227 | ms |
| memory after visiting notes — layout | 2214 | 2.361 | 3.505 | 3.741 | 16.815 | ms |
| memory after visiting notes — startup | 1 | 202.847 | 202.847 | 202.847 | 202.847 | ms |
| memory after visiting notes — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory after visiting notes — memory_after_1000_notes | 1 | 255098880.000 | 255098880.000 | 255098880.000 | 255098880.000 | bytes |
| memory after visiting notes — memory_after_100_notes | 1 | 141885440.000 | 141885440.000 | 141885440.000 | 141885440.000 | bytes |
| memory after visiting notes — memory_after_10_notes | 1 | 131219456.000 | 131219456.000 | 131219456.000 | 131219456.000 | bytes |
| memory after visiting notes — memory_visible_layout | 1 | 71073792.000 | 71073792.000 | 71073792.000 | 71073792.000 | bytes |
| memory empty editor — frame_interval | 2 | 8.930 | 9.549 | 9.549 | 9.549 | ms |
| memory empty editor — layout | 4 | 0.049 | 0.126 | 0.126 | 0.126 | ms |
| memory empty editor — startup | 2 | 171.054 | 201.845 | 201.845 | 201.845 | ms |
| memory empty editor — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory empty editor — memory_idle_30s | 2 | 85622784.000 | 86245376.000 | 86245376.000 | 86245376.000 | bytes |
| memory empty editor — memory_visible_layout | 2 | 70975488.000 | 71106560.000 | 71106560.000 | 71106560.000 | bytes |
| memory empty work folder — frame_interval | 2 | 8.938 | 9.883 | 9.883 | 9.883 | ms |
| memory empty work folder — layout | 4 | 0.060 | 0.120 | 0.120 | 0.120 | ms |
| memory empty work folder — startup | 2 | 180.951 | 187.324 | 187.324 | 187.324 | ms |
| memory empty work folder — file_open | 2 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory empty work folder — memory_visible_layout | 2 | 71024640.000 | 71041024.000 | 71041024.000 | 71041024.000 | bytes |
| memory empty work folder — memory_work_folder_idle | 2 | 77185024.000 | 77217792.000 | 77217792.000 | 77217792.000 | bytes |
| memory long-running switches, edits, and images — keystroke_to_model | 200 | 0.000 | 0.002 | 0.016 | 0.023 | ms |
| memory long-running switches, edits, and images — keystroke_to_frame | 200 | 4.023 | 7.833 | 7.844 | 7.875 | ms |
| memory long-running switches, edits, and images — frame_interval | 82 | 42.744 | 91.040 | 1014.430 | 1014.430 | ms |
| memory long-running switches, edits, and images — layout | 83 | 0.764 | 1.506 | 1.773 | 1.773 | ms |
| memory long-running switches, edits, and images — startup | 1 | 191.118 | 191.118 | 191.118 | 191.118 | ms |
| memory long-running switches, edits, and images — file_open | 1 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| memory long-running switches, edits, and images — block_index_update | 200 | 0.006 | 0.008 | 0.020 | 0.025 | ms |
| memory long-running switches, edits, and images — block_index_reparsed_bytes | 200 | 1024.000 | 1025.000 | 1025.000 | 1025.000 | bytes |
| memory long-running switches, edits, and images — block_index_invalidated_blocks | 200 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| memory long-running switches, edits, and images — memory_longrun_cycle_1 | 1 | 101154816.000 | 101154816.000 | 101154816.000 | 101154816.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_10 | 1 | 108691456.000 | 108691456.000 | 108691456.000 | 108691456.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_15 | 1 | 108707840.000 | 108707840.000 | 108707840.000 | 108707840.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_20 | 1 | 108855296.000 | 108855296.000 | 108855296.000 | 108855296.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_cycle_5 | 1 | 108281856.000 | 108281856.000 | 108281856.000 | 108281856.000 | bytes |
| memory long-running switches, edits, and images — memory_longrun_idle | 1 | 108118016.000 | 108118016.000 | 108118016.000 | 108118016.000 | bytes |
| memory long-running switches, edits, and images — memory_visible_layout | 1 | 70942720.000 | 70942720.000 | 70942720.000 | 70942720.000 | bytes |
| normal ASCII input — keystroke_to_model | 30 | 0.011 | 0.018 | 0.025 | 0.025 | ms |
| normal ASCII input — keystroke_to_frame | 30 | 4.778 | 5.389 | 5.430 | 5.430 | ms |
| normal ASCII input — frame_interval | 62 | 11.765 | 22.090 | 1509.358 | 1509.358 | ms |
| normal ASCII input — layout | 62 | 0.259 | 0.583 | 0.599 | 0.599 | ms |
| normal ASCII input — startup | 2 | 174.538 | 211.957 | 211.957 | 211.957 | ms |
| normal ASCII input — file_open | 2 | 64.616 | 72.377 | 72.377 | 72.377 | ms |
| normal ASCII input — block_index_update | 30 | 3.236 | 3.869 | 3.901 | 3.901 | ms |
| normal ASCII input — block_index_reparsed_bytes | 30 | 1048596.000 | 1048610.000 | 1048611.000 | 1048611.000 | bytes |
| normal ASCII input — block_index_invalidated_blocks | 30 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| normal ASCII input — memory_load | 2 | 70189056.000 | 70336512.000 | 70336512.000 | 70336512.000 | bytes |
| real Japanese IME composition to commit — keystroke_to_model | 240 | 0.005 | 0.008 | 0.011 | 0.017 | ms |
| real Japanese IME composition to commit — keystroke_to_frame | 240 | 4.093 | 5.774 | 7.650 | 16.753 | ms |
| real Japanese IME composition to commit — frame_interval | 273 | 11.234 | 20.047 | 33.589 | 1489.958 | ms |
| real Japanese IME composition to commit — layout | 273 | 0.262 | 0.395 | 0.496 | 0.614 | ms |
| real Japanese IME composition to commit — startup | 2 | 169.636 | 190.864 | 190.864 | 190.864 | ms |
| real Japanese IME composition to commit — file_open | 2 | 65.732 | 76.643 | 76.643 | 76.643 | ms |
| real Japanese IME composition to commit — block_index_update | 240 | 2.945 | 3.369 | 4.369 | 5.202 | ms |
| real Japanese IME composition to commit — block_index_reparsed_bytes | 240 | 1048759.000 | 1048879.000 | 1048891.000 | 1048894.000 | bytes |
| real Japanese IME composition to commit — block_index_invalidated_blocks | 240 | 0.000 | 0.000 | 0.000 | 0.000 | blocks |
| real Japanese IME composition to commit — ime_commit_to_model | 30 | 0.004 | 0.006 | 0.006 | 0.006 | ms |
| real Japanese IME composition to commit — ime_commit_to_frame | 30 | 3.708 | 4.254 | 5.041 | 5.041 | ms |
| real Japanese IME composition to commit — memory_load | 2 | 69910528.000 | 70041600.000 | 70041600.000 | 70041600.000 | bytes |
| small document warm startup — frame_interval | 18 | 12.189 | 13.437 | 13.437 | 13.437 | ms |
| small document warm startup — layout | 48 | 1.898 | 1.959 | 14.924 | 14.924 | ms |
| small document warm startup — startup | 30 | 171.249 | 183.681 | 192.808 | 192.808 | ms |
| small document warm startup — file_open | 30 | 60.512 | 63.531 | 68.510 | 68.510 | ms |
| small document warm startup — memory_load | 30 | 67747840.000 | 67862528.000 | 67895296.000 | 67895296.000 | bytes |

## Work folder scan completion

| Scenario | Indexed Markdown files | Samples | Median | p95 | Max | Unit |
|---|---:|---:|---:|---:|---:|---|
| memory_folder_10k | 10000 | 2 | 223.683 | 224.301 | 224.301 | ms |
| memory_folder_1k | 1000 | 2 | 185.856 | 201.220 | 201.220 | ms |
| memory_folder_empty | 0 | 2 | 197.590 | 206.944 | 206.944 | ms |
| memory_large_then_small | 2 | 1 | 250.144 | 250.144 | 250.144 | ms |
| memory_longrun | 2 | 1 | 243.787 | 243.787 | 243.787 | ms |
| memory_visits_1k | 1000 | 1 | 276.759 | 276.759 | 276.759 | ms |
| startup_folder_10k | 10000 | 30 | 186.574 | 198.799 | 213.406 | ms |
| startup_folder_1k | 1000 | 30 | 184.683 | 189.622 | 200.357 | ms |
| startup_folder_empty | 0 | 30 | 182.441 | 196.329 | 196.332 | ms |
