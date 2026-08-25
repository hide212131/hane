# Phase 4 UI measurement results

- Git: `3efbaacf5a5d97387ae746a1e3971239f28be0b0`
- Profile: `release`
- Rust: `rustc 1.93.1 (01f6ddf75 2026-02-11) (Homebrew)`
- GPUI: `0.2.2`
- OS: `26.5.1`
- CPU: `Apple M3 Pro`
- Input sources: `com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese, com.apple.keylayout.ABC`
- Refresh rate: `variable (CGDisplayMode reports 0)` Hz
- Background job states: `false, true`

| Scenario / metric | Samples | Median | p95 | p99 | Max | Unit |
|---|---:|---:|---:|---:|---:|---|
| 100 MB input at end — keystroke_to_model | 30 | 0.005 | 0.009 | 0.010 | 0.010 | ms |
| 100 MB input at end — keystroke_to_frame | 30 | 2.811 | 3.564 | 7.648 | 7.648 | ms |
| 100 MB input at end — frame_interval | 30 | 25.006 | 31.177 | 547.895 | 547.895 | ms |
| 100 MB input at end — layout | 30 | 1.611 | 1.697 | 1.709 | 1.709 | ms |
| 100 MB input at end — startup | 1 | 310.500 | 310.500 | 310.500 | 310.500 | ms |
| 100 MB input at end — file_open | 1 | 133.333 | 133.333 | 133.333 | 133.333 | ms |
| 100 MB input at end — memory_load | 1 | 227098624.000 | 227098624.000 | 227098624.000 | 227098624.000 | bytes |
| 100 MB input at middle — keystroke_to_model | 30 | 0.006 | 0.008 | 0.008 | 0.008 | ms |
| 100 MB input at middle — keystroke_to_frame | 30 | 4.356 | 5.183 | 5.215 | 5.215 | ms |
| 100 MB input at middle — frame_interval | 30 | 24.988 | 33.143 | 548.359 | 548.359 | ms |
| 100 MB input at middle — layout | 30 | 1.529 | 1.600 | 1.629 | 1.629 | ms |
| 100 MB input at middle — startup | 1 | 280.165 | 280.165 | 280.165 | 280.165 | ms |
| 100 MB input at middle — file_open | 1 | 124.578 | 124.578 | 124.578 | 124.578 | ms |
| 100 MB input at middle — memory_load | 1 | 226967552.000 | 226967552.000 | 226967552.000 | 226967552.000 | bytes |
| 100 MB input at start — keystroke_to_model | 30 | 0.005 | 0.007 | 0.007 | 0.007 | ms |
| 100 MB input at start — keystroke_to_frame | 30 | 2.319 | 3.272 | 3.849 | 3.849 | ms |
| 100 MB input at start — frame_interval | 30 | 24.939 | 33.366 | 578.070 | 578.070 | ms |
| 100 MB input at start — layout | 30 | 0.130 | 0.138 | 0.141 | 0.141 | ms |
| 100 MB input at start — startup | 1 | 363.665 | 363.665 | 363.665 | 363.665 | ms |
| 100 MB input at start — file_open | 1 | 160.065 | 160.065 | 160.065 | 160.065 | ms |
| 100 MB input at start — memory_load | 1 | 227000320.000 | 227000320.000 | 227000320.000 | 227000320.000 | bytes |
| 100 MB input combined — keystroke_to_model | 90 | 0.006 | 0.007 | 0.010 | 0.010 | ms |
| 100 MB input combined — keystroke_to_frame | 90 | 2.694 | 4.975 | 7.648 | 7.648 | ms |
| 100 MB input combined — layout | 90 | 1.529 | 1.667 | 1.709 | 1.709 | ms |
| 100 MB input while scrolling — keystroke_to_model | 30 | 0.005 | 0.007 | 0.007 | 0.007 | ms |
| 100 MB input while scrolling — keystroke_to_frame | 30 | 2.411 | 4.676 | 4.679 | 4.679 | ms |
| 100 MB input while scrolling — frame_interval | 236 | 8.312 | 9.213 | 9.379 | 9.475 | ms |
| 100 MB input while scrolling — layout | 236 | 0.144 | 0.220 | 0.234 | 0.239 | ms |
| 100 MB input while scrolling — startup | 1 | 275.659 | 275.659 | 275.659 | 275.659 | ms |
| 100 MB input while scrolling — file_open | 1 | 124.982 | 124.982 | 124.982 | 124.982 | ms |
| 100 MB input while scrolling — memory_load | 1 | 227000320.000 | 227000320.000 | 227000320.000 | 227000320.000 | bytes |
| 100 MB scroll only — frame_interval | 189 | 8.323 | 9.038 | 9.273 | 9.421 | ms |
| 100 MB scroll only — layout | 189 | 0.331 | 0.438 | 0.453 | 0.454 | ms |
| 100 MB scroll only — startup | 1 | 268.957 | 268.957 | 268.957 | 268.957 | ms |
| 100 MB scroll only — file_open | 1 | 123.595 | 123.595 | 123.595 | 123.595 | ms |
| 100 MB scroll only — memory_load | 1 | 227000320.000 | 227000320.000 | 227000320.000 | 227000320.000 | bytes |
| 100k paragraphs input at end — keystroke_to_model | 30 | 0.006 | 0.012 | 0.016 | 0.016 | ms |
| 100k paragraphs input at end — keystroke_to_frame | 30 | 4.034 | 5.362 | 6.093 | 6.093 | ms |
| 100k paragraphs input at end — frame_interval | 32 | 25.093 | 124.490 | 450.227 | 450.227 | ms |
| 100k paragraphs input at end — layout | 32 | 1.700 | 1.914 | 3.152 | 3.152 | ms |
| 100k paragraphs input at end — startup | 1 | 174.214 | 174.214 | 174.214 | 174.214 | ms |
| 100k paragraphs input at end — file_open | 1 | 15.500 | 15.500 | 15.500 | 15.500 | ms |
| 100k paragraphs input at end — memory_load | 1 | 65961984.000 | 65961984.000 | 65961984.000 | 65961984.000 | bytes |
| 100k paragraphs input at middle — keystroke_to_model | 30 | 0.009 | 0.021 | 0.035 | 0.035 | ms |
| 100k paragraphs input at middle — keystroke_to_frame | 30 | 6.779 | 7.219 | 9.622 | 9.622 | ms |
| 100k paragraphs input at middle — frame_interval | 32 | 25.015 | 123.932 | 428.260 | 428.260 | ms |
| 100k paragraphs input at middle — layout | 32 | 1.551 | 1.668 | 3.224 | 3.224 | ms |
| 100k paragraphs input at middle — startup | 1 | 186.801 | 186.801 | 186.801 | 186.801 | ms |
| 100k paragraphs input at middle — file_open | 1 | 15.460 | 15.460 | 15.460 | 15.460 | ms |
| 100k paragraphs input at middle — memory_load | 1 | 65732608.000 | 65732608.000 | 65732608.000 | 65732608.000 | bytes |
| 100k paragraphs input at start — keystroke_to_model | 30 | 0.007 | 0.010 | 0.012 | 0.012 | ms |
| 100k paragraphs input at start — keystroke_to_frame | 30 | 2.447 | 5.302 | 5.671 | 5.671 | ms |
| 100k paragraphs input at start — frame_interval | 32 | 25.075 | 114.178 | 518.404 | 518.404 | ms |
| 100k paragraphs input at start — layout | 32 | 0.152 | 0.165 | 0.165 | 0.165 | ms |
| 100k paragraphs input at start — startup | 1 | 190.390 | 190.390 | 190.390 | 190.390 | ms |
| 100k paragraphs input at start — file_open | 1 | 20.098 | 20.098 | 20.098 | 20.098 | ms |
| 100k paragraphs input at start — memory_load | 1 | 66060288.000 | 66060288.000 | 66060288.000 | 66060288.000 | bytes |
| 100k paragraphs input while scrolling — keystroke_to_model | 30 | 0.007 | 0.012 | 0.012 | 0.012 | ms |
| 100k paragraphs input while scrolling — keystroke_to_frame | 30 | 2.283 | 3.846 | 4.686 | 4.686 | ms |
| 100k paragraphs input while scrolling — frame_interval | 229 | 8.314 | 9.427 | 10.312 | 10.729 | ms |
| 100k paragraphs input while scrolling — layout | 229 | 0.178 | 0.438 | 0.545 | 0.985 | ms |
| 100k paragraphs input while scrolling — startup | 1 | 162.521 | 162.521 | 162.521 | 162.521 | ms |
| 100k paragraphs input while scrolling — file_open | 1 | 17.466 | 17.466 | 17.466 | 17.466 | ms |
| 100k paragraphs input while scrolling — memory_load | 1 | 67239936.000 | 67239936.000 | 67239936.000 | 67239936.000 | bytes |
| 100k paragraphs scroll only — frame_interval | 188 | 8.332 | 9.191 | 9.551 | 9.683 | ms |
| 100k paragraphs scroll only — layout | 188 | 0.337 | 0.443 | 0.553 | 0.615 | ms |
| 100k paragraphs scroll only — startup | 1 | 181.714 | 181.714 | 181.714 | 181.714 | ms |
| 100k paragraphs scroll only — file_open | 1 | 14.050 | 14.050 | 14.050 | 14.050 | ms |
| 100k paragraphs scroll only — memory_load | 1 | 64454656.000 | 64454656.000 | 64454656.000 | 64454656.000 | bytes |
| empty cold startup (OS cache not purged) — frame_interval | 20 | 16.052 | 22.866 | 23.874 | 23.874 | ms |
| empty cold startup (OS cache not purged) — layout | 50 | 0.048 | 0.068 | 0.104 | 0.104 | ms |
| empty cold startup (OS cache not purged) — startup | 30 | 174.552 | 207.767 | 230.619 | 230.619 | ms |
| empty cold startup (OS cache not purged) — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty cold startup (OS cache not purged) — memory_ready | 30 | 64241664.000 | 64421888.000 | 64487424.000 | 64487424.000 | bytes |
| empty warm startup — frame_interval | 13 | 16.528 | 24.131 | 24.131 | 24.131 | ms |
| empty warm startup — layout | 43 | 0.048 | 0.056 | 0.227 | 0.227 | ms |
| empty warm startup — startup | 30 | 165.934 | 192.138 | 203.146 | 203.146 | ms |
| empty warm startup — file_open | 30 | 0.000 | 0.000 | 0.000 | 0.000 | ms |
| empty warm startup — memory_ready | 30 | 64323584.000 | 64471040.000 | 64520192.000 | 64520192.000 | bytes |
| input during background presentation update — keystroke_to_model | 30 | 0.005 | 0.007 | 0.007 | 0.007 | ms |
| input during background presentation update — keystroke_to_frame | 30 | 0.635 | 2.615 | 3.468 | 3.468 | ms |
| input during background presentation update — frame_interval | 250 | 8.304 | 8.798 | 9.322 | 9.446 | ms |
| input during background presentation update — layout | 250 | 0.114 | 0.131 | 0.146 | 0.164 | ms |
| input during background presentation update — startup | 1 | 182.514 | 182.514 | 182.514 | 182.514 | ms |
| input during background presentation update — file_open | 1 | 35.817 | 35.817 | 35.817 | 35.817 | ms |
| input during background presentation update — memory_load | 1 | 60964864.000 | 60964864.000 | 60964864.000 | 60964864.000 | bytes |
| memory 10 MB — frame_interval | 3 | 58.963 | 338.572 | 338.572 | 338.572 | ms |
| memory 10 MB — layout | 4 | 0.122 | 0.179 | 0.179 | 0.179 | ms |
| memory 10 MB — startup | 1 | 195.946 | 195.946 | 195.946 | 195.946 | ms |
| memory 10 MB — file_open | 1 | 32.646 | 32.646 | 32.646 | 32.646 | ms |
| memory 10 MB — memory_load | 1 | 81133568.000 | 81133568.000 | 81133568.000 | 81133568.000 | bytes |
| memory 10 MB — memory_visible_layout | 1 | 89260032.000 | 89260032.000 | 89260032.000 | 89260032.000 | bytes |
| memory 10 MB — memory_idle_30s | 1 | 103579648.000 | 103579648.000 | 103579648.000 | 103579648.000 | bytes |
| memory 100 MB — frame_interval | 3 | 79.396 | 3241.784 | 3241.784 | 3241.784 | ms |
| memory 100 MB — layout | 4 | 0.137 | 0.211 | 0.211 | 0.211 | ms |
| memory 100 MB — startup | 1 | 325.703 | 325.703 | 325.703 | 325.703 | ms |
| memory 100 MB — file_open | 1 | 144.340 | 144.340 | 144.340 | 144.340 | ms |
| memory 100 MB — memory_load | 1 | 227180544.000 | 227180544.000 | 227180544.000 | 227180544.000 | bytes |
| memory 100 MB — memory_visible_layout | 1 | 233684992.000 | 233684992.000 | 233684992.000 | 233684992.000 | bytes |
| memory 100 MB — memory_idle_30s | 1 | 263061504.000 | 263061504.000 | 263061504.000 | 263061504.000 | bytes |
| normal ASCII input — keystroke_to_model | 31 | 0.007 | 0.012 | 0.012 | 0.012 | ms |
| normal ASCII input — keystroke_to_frame | 31 | 2.570 | 4.104 | 4.149 | 4.149 | ms |
| normal ASCII input — frame_interval | 35 | 25.464 | 92.961 | 560.231 | 560.231 | ms |
| normal ASCII input — layout | 35 | 0.137 | 0.258 | 0.269 | 0.269 | ms |
| normal ASCII input — startup | 1 | 201.882 | 201.882 | 201.882 | 201.882 | ms |
| normal ASCII input — file_open | 1 | 14.124 | 14.124 | 14.124 | 14.124 | ms |
| normal ASCII input — memory_load | 1 | 60735488.000 | 60735488.000 | 60735488.000 | 60735488.000 | bytes |
| real Japanese IME composition to commit — keystroke_to_model | 240 | 0.007 | 0.010 | 0.013 | 0.020 | ms |
| real Japanese IME composition to commit — keystroke_to_frame | 240 | 1.633 | 3.414 | 4.419 | 6.125 | ms |
| real Japanese IME composition to commit — frame_interval | 242 | 10.443 | 37.022 | 82.324 | 484.869 | ms |
| real Japanese IME composition to commit — layout | 242 | 0.151 | 0.304 | 0.324 | 0.359 | ms |
| real Japanese IME composition to commit — startup | 1 | 198.980 | 198.980 | 198.980 | 198.980 | ms |
| real Japanese IME composition to commit — file_open | 1 | 13.498 | 13.498 | 13.498 | 13.498 | ms |
| real Japanese IME composition to commit — ime_commit_to_model | 30 | 0.006 | 0.013 | 0.014 | 0.014 | ms |
| real Japanese IME composition to commit — ime_commit_to_frame | 30 | 1.577 | 5.300 | 6.125 | 6.125 | ms |
| real Japanese IME composition to commit — memory_load | 1 | 62439424.000 | 62439424.000 | 62439424.000 | 62439424.000 | bytes |

