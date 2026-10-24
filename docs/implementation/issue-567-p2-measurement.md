# RF2-A P2: EditorViewの計測用メソッドを機械的に分離する

対象: Issue #567（親 #297）。基準main: `68c438aea3ce39e12b25aa4ad3e2f5e16294190d`。作成日: 2026-10-25 JST。

## 目的と開始条件

P1のrender分離はPR #583で実装・受入・merge済み。本P2は残る計測接続を見分けられる配置にする一責務の移動であり、計測値の改善・仕様変更をしない。#298の台帳・保護契約と#300の該当領域の維持判断を継承する。RF0全体の完了待ちは要求しない。

観測時にopenのRF実装PRはない。機能PR #604（リンククリック）、#606（list indent）、#609（外部フォルダopen）、#610（フォルダ内作成）とは今回の計測impl本体を共有しない。親Viewのmodule宣言近傍と後続mainの変更は着手時に再確認する。機能追加、open/save、新規作成、リンク判定を本P2へ混ぜない。

この文書を作る準備commitは文書のみ。製品実装、build/test、GUI、性能測定は未実施である。実装後の結果はPRで対象head/baseと対応させ、この作成時点の記録を現在の実行結果として使わない。

## 正確な移動範囲

`crates/ui/src/view.rs`から次の二つのimplを、属性・コメント・順序を含めprivateな`crates/ui/src/view/measurement.rs`へ移す。新moduleは`use super::*;`を使い、親に**無条件**の`mod measurement;`を追加する。module自体をfeatureで隠すと通常構成のno-opが失われるため、cfgは各implとメソッドに現状どおり残す。

| 基準source | 移動するもの | 基準範囲のSHA-256 |
| --- | --- | --- |
| view.rs 6443〜6698行、9,938 bytes | `#[cfg(any(feature = "instrument", feature = "timing-probe"))]`付きimpl全体（17メソッド） | `d05cf8f02d121a14129e42adabe40757ae0b8c76e30f1ce20a716ba8fa9c63a4` |
| view.rs 6749〜6764行、510 bytes | `#[cfg(not(any(feature = "instrument", feature = "timing-probe")))]`付きimpl全体（4 no-op） | `67766181071e484a1913e9b8125dbdb62d46b534866f2194cc02a57c18053caf` |

hash範囲は属性の先頭から次のtop-level関数/impl直前までの終端空行を含む。行番号/hashは上記基準の識別であり、実装直前のbaseが変われば再抽出する。

計測有効側の17メソッド:

- `arm_startup_timing`
- `record_phase0_idle_memory`
- `measurement_work_folder_state`
- `measurement_work_folder_note_paths`
- `measurement_note_is_loaded`
- `record_measurement_memory`
- `measurement_elapsed_ms`
- `run_measurement_edit_cycles`
- `apply_phase0_background_presentation`
- `enable_display_linked_scroll_measurement`
- `apply_phase1_scroll_frame`
- `step_measurement_scroll`
- `set_cursor_offset_for_measurement`
- `move_cursor_down_for_development`
- `record_block_index_update`
- `record_scroll_receipt_for_measurement`
- `record_frame_instrumentation`

通常構成の4 no-opは`step_measurement_scroll`、`record_block_index_update`、`record_scroll_receipt_for_measurement`、`record_frame_instrumentation`。

### 可視性と名前解決

`record_block_index_update`は親の`after_input`から呼ばれるため、**両cfg側**でprivateから`pub(super)`へ変える。これは親から使うための局所的な可視性調整で、外部公開APIではない。`apply_phase1_scroll_frame`は同module内からのみ呼ぶためprivateのまま。他の既存`pub`/`pub(crate)`は維持し、moduleは公開しない。

その他は本文・属性・コメントを原文どおり移す。親importを減らす場合は他の子moduleの`use super::*`も含め利用を確認する。可視性調整以外の名前解決変更が必要なら範囲と理由をPRへ示し、意味変更へ広げない。

## callerと状態所有

| 利用者 | 維持する接続 |
| --- | --- |
| `crates/app/src/main.rs` | 起動時`arm_startup_timing` |
| `crates/app/src/instrument.rs` | work-folder走査結果、session保持量、RSS、編集cycle、背景表示generation、scroll計測、caret移動の既存API |
| 親Viewの`after_input` | `record_block_index_update`。呼出位置・回数を維持 |
| `view/render.rs` | `step_measurement_scroll`。render内の位置を維持 |
| `view/viewport.rs`、親Viewの既存test | `record_scroll_receipt_for_measurement` |
| `crates/ui/src/capture.rs` | `record_frame_instrumentation`。paint時点と真のpresentation時点を混同しない |

`EditorView`のfield、`instrumentation`、`metrics`、初期化、feature別型alias、`InstrumentationState`と出力型を持つ`crates/ui/src/instrument.rs`は動かさない。session/work_folder/loading_paths、heights、viewport_height、scroll_y、background_presentation_generationの所有者と寿命も不変。dispatch/after_input、frame要求、エラー処理、metrics出力順とcache hit/miss reset位置、最初のscroll receiptだけを保つ規則を維持する。`measurement_elapsed_ms`に現行Rust呼出元が見つからなくても、このPRでは削除しない。

## 残るRF2-Aの扱い

| 残る領域 | 本P2での分類と境界 |
| --- | --- |
| render組立 | P1 #583で処理済み。再移動しない |
| 計測用impl / no-op | 今回移動する |
| 型・field・初期化 / from_sessions | 親へ維持。今回のメソッド移動を理由に状態所有を変えない。RF2-A全体の維持判断は残る |
| settings画面・設定操作 | 今回維持、後続の配置棚卸し対象。別機能の仕様変更と分ける |
| height/cache共有helper・geometry・可視block | 今回維持。配置整理と#306/#307/#368の意味・寿命変更を分ける |
| pointer/panel resize・scrollbar / viewport navigation | 今回維持。既存viewport子moduleとの残る対応を後続で照合 |
| open/save/draft/H1、work-folder/session/tab接続 | 既存session_save等を利用し、親に残る接続の棚卸しは後続。#308/#309や実装中機能と並行で同じ状態を変えない |
| AI/search/update/filter/rename等の既存子module、actions/input/line/shape | 既存配置を再利用。不要な再分割をしない |
| 既存testsと共通helper | 今回親に維持。入力・期待値・属性・本体・module pathを変更/削減しない |

この表はP2の境界であり、RF2-A全対象の調査・受入完了ではない。残る領域は#567の後続でcurrent codeを棚卸しし、移動・処理済み・根拠付き維持・別担当の意味変更を確定する。P2だけで#567/#297をcloseしない。

## 検証と受入

1. 実装時baseと移動後の二つのimplを比較する。`record_block_index_update`の両側の`pub(super)`追加だけを戻して比較した本文・属性・コメントがbyte一致すること。旧定義がなく、各featureで対応する実装が一つだけであること。製品差分は親のmodule宣言、二つのimpl削除、新moduleとこの可視性調整に限る。
2. 既存testの名前・属性・入力・期待値・本体と公開APIを維持する。既存`scroll_receipt_for_measurement_keeps_the_first_pending_tick`は最初のreceiptを保持するassertをそのまま実行する。移動を写しただけの新testは不要。
3. current headで以下を確認し、head/base、OS、command、結果URLをPRへ残す。通常とall-featuresだけではtiming-probe単独のcfg分岐を覆わない。

```sh
cargo test --workspace --locked
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --locked -p hane-ui --features timing-probe
cargo clippy --locked -p hane-ui --all-targets --features timing-probe -- -D warnings
cargo build --release --locked -p hane
cargo build --release --locked -p hane --features timing-probe
cargo build --release --locked -p hane --features instrument
```

`instrument`はmanifest上`timing-probe`を含む。通常、timing-probe単独、instrumentの三構成で名前解決とapp側APIの接続を確認する。macOS/Windowsの関連CIを確認し、未実行構成は成功と記録しない。workspace外vendorテストは既存CIの対象範囲を維持する。

4. P2が上記移動と可視性調整だけなら、処理量・計測内容・入力/描画/geometryを変えないため、新たなnative GUI/性能再測定を受入条件に追加しない。実施済みとは記録しない。差分がこの前提を超える場合は機械的移動から分離し、Commander Policyに従って必要な検証を再判断する。#23の原本や他Issueの未実施条件は免除しない。
5. `docs/refactor-execution-plan.md`へP2の旧→新対応、維持理由、検証の実施/未実施を短く追記し、本仕様を参照する。仕様を複製しない。

## 対象外と復旧

意味変更、最適化、bug fix、metrics形式・環境変数・時刻基準変更、計測機能削除、公開API縮小、新Manager/trait/state owner、test移動、全体整形、Cargo/vendor/workflow/保護パス変更を含めない。

この一責務のPRを一単位でrevertする。後続が新moduleへ依存する場合は逆順に戻す。永続データの移行・外部設定変更は不要。

選定記録: Jev `jev-1.14.0`は`prepare_567_p2`を選択（probability 0.60、confidence 0.47）。候補は本P2準備、RF4追加調査、RF0追加調査、実装待機。Commanderはopen RF PRなし、P1 merge済み、current main、機能PRとの範囲、隔離worktreeを確認した。
