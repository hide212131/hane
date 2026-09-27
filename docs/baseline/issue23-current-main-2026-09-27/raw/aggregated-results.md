# UI measurement results

- Git: `58ab905db035e3d56ac4c59a8fbd6c2be0065f53`
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
| 100 MB input at end — startup | 1 | 357.152 | 357.152 | 357.152 | 357.152 | ms |
| 100 MB input at end — file_open | 1 | 226.805 | 226.805 | 226.805 | 226.805 | ms |
| 100 MB input at end — memory_load | 1 | 350027776.000 | 350027776.000 | 350027776.000 | 350027776.000 | bytes |
| 100 MB input at middle — startup | 1 | 347.857 | 347.857 | 347.857 | 347.857 | ms |
| 100 MB input at middle — file_open | 1 | 217.115 | 217.115 | 217.115 | 217.115 | ms |
| 100 MB input at middle — memory_load | 1 | 349929472.000 | 349929472.000 | 349929472.000 | 349929472.000 | bytes |
| 100 MB input at start — startup | 1 | 343.618 | 343.618 | 343.618 | 343.618 | ms |
| 100 MB input at start — file_open | 1 | 222.791 | 222.791 | 222.791 | 222.791 | ms |
| 100 MB input at start — memory_load | 1 | 349470720.000 | 349470720.000 | 349470720.000 | 349470720.000 | bytes |
| 100 MB input while scrolling — startup | 1 | 345.869 | 345.869 | 345.869 | 345.869 | ms |
| 100 MB input while scrolling — file_open | 1 | 219.487 | 219.487 | 219.487 | 219.487 | ms |
| 100 MB input while scrolling — memory_load | 1 | 349945856.000 | 349945856.000 | 349945856.000 | 349945856.000 | bytes |
| 100 MB scroll only — startup | 1 | 346.437 | 346.437 | 346.437 | 346.437 | ms |
| 100 MB scroll only — file_open | 1 | 222.611 | 222.611 | 222.611 | 222.611 | ms |
| 100 MB scroll only — memory_load | 1 | 350093312.000 | 350093312.000 | 350093312.000 | 350093312.000 | bytes |
| 100 MiB document warm startup — frame_interval | 27 | 12.761 | 13.324 | 13.678 | 13.678 | ms |
| 100 MiB document warm startup — layout | 57 | 16.263 | 17.574 | 18.343 | 18.343 | ms |
| 100 MiB document warm startup — startup | 30 | 343.596 | 367.273 | 367.319 | 367.319 | ms |
| 100 MiB document warm startup — file_open | 30 | 218.981 | 232.785 | 235.550 | 235.550 | ms |
| 100 MiB document warm startup — memory_load | 30 | 350027776.000 | 350142464.000 | 350142464.000 | 350142464.000 | bytes |
| 100k paragraphs input at end — startup | 1 | 175.050 | 175.050 | 175.050 | 175.050 | ms |
| 100k paragraphs input at end — file_open | 1 | 66.356 | 66.356 | 66.356 | 66.356 | ms |
| 100k paragraphs input at end — memory_load | 1 | 75431936.000 | 75431936.000 | 75431936.000 | 75431936.000 | bytes |
| 100k paragraphs input at middle — startup | 1 | 174.836 | 174.836 | 174.836 | 174.836 | ms |
| 100k paragraphs input at middle — file_open | 1 | 67.619 | 67.619 | 67.619 | 67.619 | ms |
| 100k paragraphs input at middle — memory_load | 1 | 75382784.000 | 75382784.000 | 75382784.000 | 75382784.000 | bytes |
| 100k paragraphs input at start — startup | 1 | 186.567 | 186.567 | 186.567 | 186.567 | ms |
| 100k paragraphs input at start — file_open | 1 | 71.331 | 71.331 | 71.331 | 71.331 | ms |
| 100k paragraphs input at start — memory_load | 1 | 75513856.000 | 75513856.000 | 75513856.000 | 75513856.000 | bytes |
| 100k paragraphs input while scrolling — startup | 1 | 176.160 | 176.160 | 176.160 | 176.160 | ms |
| 100k paragraphs input while scrolling — file_open | 1 | 68.277 | 68.277 | 68.277 | 68.277 | ms |
| 100k paragraphs input while scrolling — memory_load | 1 | 75268096.000 | 75268096.000 | 75268096.000 | 75268096.000 | bytes |
| 100k paragraphs scroll only — startup | 1 | 174.242 | 174.242 | 174.242 | 174.242 | ms |
| 100k paragraphs scroll only — file_open | 1 | 66.114 | 66.114 | 66.114 | 66.114 | ms |
| 100k paragraphs scroll only — memory_load | 1 | 75251712.000 | 75251712.000 | 75251712.000 | 75251712.000 | bytes |
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
| input during background presentation update — startup | 1 | 169.147 | 169.147 | 169.147 | 169.147 | ms |
| input during background presentation update — file_open | 1 | 63.734 | 63.734 | 63.734 | 63.734 | ms |
| input during background presentation update — memory_load | 1 | 70107136.000 | 70107136.000 | 70107136.000 | 70107136.000 | bytes |
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
| normal ASCII input — startup | 1 | 174.538 | 174.538 | 174.538 | 174.538 | ms |
| normal ASCII input — file_open | 1 | 64.616 | 64.616 | 64.616 | 64.616 | ms |
| normal ASCII input — memory_load | 1 | 70189056.000 | 70189056.000 | 70189056.000 | 70189056.000 | bytes |
| real Japanese IME composition to commit — startup | 1 | 169.636 | 169.636 | 169.636 | 169.636 | ms |
| real Japanese IME composition to commit — file_open | 1 | 65.732 | 65.732 | 65.732 | 65.732 | ms |
| real Japanese IME composition to commit — memory_load | 1 | 70041600.000 | 70041600.000 | 70041600.000 | 70041600.000 | bytes |
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
