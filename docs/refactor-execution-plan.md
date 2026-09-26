# Hane リファクタリング実施計画 — 第2サイクル

作成日: 2026年9月23日  
親Issue: [#297](https://github.com/hide212131/hane/issues/297)  
詳細計画: [refactor-plan.md](refactor-plan.md)

## 1. 文書の役割と現在地

詳細計画は目標・範囲・契約を定め、この文書は実行Issue、依存関係、要件の割当、提出物を定める。各Issue本文は担当範囲の具体的手順・非目標・検証・受入条件を持つ。計画の調査基準は `ef9356c52bae06dee6cf49fa7b1a59ce004b52a0` であり、着手時にはcurrent codeと関連Issue/PR/ADRを読み直す。

**登録時点: 親Issue 1件、実行Issue 18件を作成。RF5-BのIssue作成だけはAPIに拒否され、未作成であった。本文を第7節に保持した。実装・製品テスト・GUI検証・性能測定は未着手であり、RF0を含めて完了したフェーズはない。**

2026年9月26日に RF5-B を [#368](https://github.com/hide212131/hane/issues/368) として登録した。以後は #368 を正本の実行Issueとして追跡し、登録済みであることを実施済み・検証済みとは扱わない。

文書保存ブランチは `docs/refactor-cycle-2-20260923`。文書PRのmerge前はそのブランチの第2サイクル文書を参照する。mainへの反映前に旧R0〜R5の文書を第2サイクルと取り違えない。Issueの現在の状態と実装PRの証拠はGitHubを正とし、この登録時点の説明を実行状態として使わない。

策定済み詳細計画は全文を変更せず保存した。元ファイル名は `hane-refactor-plan-2026-09-23.md`、35,652 bytes、Git blobは `06ac2536674e13fd59f637f95b4fc6e950b1e336`、SHA-256は `0889f7c0ec0354c1f50c2b8f36e2e7d579f8d520994d20be29b6bd509d6ed8e7`。今後の意図した変更は通常のGit履歴で追跡し、元の調査日・根拠を現行実測へ書き換えない。

旧R0〜R5の計画と実施記録は [第1サイクルの履歴](history/refactor-cycle-1/README.md) に原文のまま保存する。旧計画の「完了」は今回の完了を意味しない。

## 2. Issue一覧と依存関係

以下の依存は単なる番号順ではなく、受け入れ済みの契約・変更領域を示す。同じ責務を変える作業を重ねず、無関係なmainの更新や別領域の作業は止めない。

| 作業ID | 実行Issue | 担当範囲 | 開始前に必要なもの |
|---|---|---|---|
| RF0 | [#298](https://github.com/hide212131/hane/issues/298) | 全域台帳、保護契約、既知問題、テスト不足、文書照合 | 本計画の確認。最初に着手する。 |
| RF0-P | [#23（既存）](https://github.com/hide212131/hane/issues/23) | 起動・入力/scroll・RSS・work folder・session保持の基準 | #298と条件をそろえ、比較対象を変える前に測定する。 |
| RF1-A | [#299](https://github.com/hide212131/hane/issues/299) | 旧AADW経路を到達性で確認して削除 | #298の現行入口・台帳・契約 |
| RF1-B | [#300](https://github.com/hide212131/hane/issues/300) | 製品側未使用API、実験、設定、fixture/assets | #298の対象台帳・構成別利用確認 |
| RF2-A | [#301](https://github.com/hide212131/hane/issues/301) | EditorViewの責務別機械的分割 | #298と#300の当該領域。描画競合は第3節も確認。 |
| RF2-B | [#302](https://github.com/hide212131/hane/issues/302) | presentationの機械的分割 | #298と#300の当該領域。公開APIを維持。 |
| RF2-C | [#303](https://github.com/hide212131/hane/issues/303) | Markdownの機械的分割 | #298と#300の当該領域。公開APIを維持。 |
| RF3-A | [#304](https://github.com/hide212131/hane/issues/304) | 本文/filter/renameの入力先判定 | #301の入力境界と#298の操作契約 |
| RF3-B | [#305](https://github.com/hide212131/hane/issues/305) | 短い入力欄の文字列・selection操作 | #301/#304の受入済み境界 |
| RF4 | [#306](https://github.com/hide212131/hane/issues/306) | snapshot、解析/projection経路、引数、fallback | #301/#302/#303の該当分割、第3節の競合確認 |
| RF5-A | [#307](https://github.com/hide212131/hane/issues/307) | BlockLayoutを正とするgeometry接続 | #306、関連するRF3/RF6の接続、第3節 |
| RF5-B | [#368](https://github.com/hide212131/hane/issues/368) | cache/高さ索引/background jobの所有者と寿命 | #301/#302/#306、#309の文書切替境界、#23の基準。#307と競合させない。 |
| RF6-A | [#308](https://github.com/hide212131/hane/issues/308) | H1命名/renameの状態と調停 | #301の保存/rename分割、#298の保護契約 |
| RF6-B | [#309](https://github.com/hide212131/hane/issues/309) | open/save/draft、要求と結果受理 | #301/#298、#308と共有する状態の受入済み境界 |
| RF7-A | [#310](https://github.com/hide212131/hane/issues/310) | 現行GUI検証の重複整理 | #298の入口。#299と共有する部分はその整理後。 |
| RF7-B | [#311](https://github.com/hide212131/hane/issues/311) | CIと変更範囲、構成別検証 | #298。#299/#310と同じ入口は受入境界を先に調整。 |
| RF7-C | [#312](https://github.com/hide212131/hane/issues/312) | 既存release/version整合と手順 | #298。#311と共有checkを変更する場合は調整。 |
| RF7-D | [#313](https://github.com/hide212131/hane/issues/313) | 依存/feature/vendor patchと解除条件 | #298/#300の利用根拠 |
| RF8-A | [#314](https://github.com/hide212131/hane/issues/314) | 実測で立証した局所最適化 | #23の基準、対象に関係するRF1〜RF7の受入 |
| RF8-B | [#315](https://github.com/hide212131/hane/issues/315) | 移行残骸撤去、全要件の最終証拠、文書整合 | 全対象Issueと#23、RF5-B #368を含む全範囲の受入 |

原則の順序は `RF0 → RF1 → RF2 → RF3/RF4/RF6 → RF5 → RF8`。RF7はRF0の現行入口確認後、製品と競合しない範囲で進める。RF0全体が未完でも、独立領域について台帳・契約・必要な検証条件を受け入れた部分引き渡しはできる。その範囲と残件をIssueに明記し、RF0全体を完了にしない。性能を変え得る実装は、該当する変更前測定を省略しない。

一つのIssueが複数PRになることは許容する。一責務の移動、意味変更、最適化、機能追加を一つのPRに混ぜない。未完の大規模移行を抱えた長期ブランチは作らず、各PRで検証可能な状態を保つ。

<a id="feature-development-policy"></a>

### 2.1 機能追加を続けるための着手手順

この節は [AGENTS.md の共通方針](../AGENTS.md) を、機能追加担当が実行するための手順である。機能追加は継続し、全体の完了待ちはしない。第2節の RF0〜RF8 の依存表はリファクタリング作業間の関係を示すもので、すべての機能追加に同じ待ち条件を課すものではない。

着手前に current main と関係する機能・リファクタリングの Issue / PR を読み、変更する責務、共有する状態、守る契約を確認する。別ファイルでも同じ状態を変えるなら調整し、同じファイルにあるという理由だけで全機能を止めない。

| 確認した関係 | 進め方 |
|---|---|
| 改修中の責務・状態・契約と重ならず、既存の仕組みで実装できる | 機能追加を進める。依存なしの根拠と、その機能に必要な検証を記す。 |
| 入力・表示・状態管理などが重なり、そのまま追加すると重複や手戻りが増える | 関係する最小範囲の整理を先に、機能追加とは別 PR で行う。受入・main への反映を確認してから、その経路を使って機能を追加する。無関係なフェーズは待たない。 |
| source の保持、保存形式、IME、位置対応などの前提を変える | 関係する設計・保護契約と検証を先に確定する。安全性を未確認のまま進めず、待つ対象は必要な責務に限定する。 |

機能 Issue の着手メモには「変更する責務」「関連 Issue / PR」「先に必要な範囲と理由、または依存なしの根拠」「使う受入済みの経路と検証」を短く記す。新しい様式・ラベル・管理システムは必須にしない。worker が依存や決定済みの制約との矛盾を見つけた場合は、無断で範囲を変えず報告する。

同じ責務の大きな移動と機能追加を同時に進めない。必要な整理を小さく受け入れて機能開発へ渡し、移動・意味変更・最適化・機能追加を同じ PR に混ぜない。不要な全体直列化や長期移行ブランチは避ける。受入済みの経路を使い、旧経路への追加や新旧両方への同一ルールの実装は行わない。

例えばサイドバーのキーボード操作 #237 は、入力先判定 #304 と関係する。着手時のコードに照らして入力関連の必要な契約・分割・判定整理を先行させ、別 PR で機能を追加する。保存・vendor・リリースの整理や #297 全体の完了までは待たない。これは固定の全体依存ではなく、必要な範囲を確認する例である。

次に追加する機能と、そのための整理を組み合わせて優先順位を決める。機能追加だけでは着手しにくい旧経路の削除なども、小さな PR で継続する。固定の工数比率は設けない。

本サイクルの完了対象は RF0 で確認・合意した改善範囲として追跡し、新しい機能要求を自動的に全件取り込んで拡大し続けない。ただし新機能が整理済みの責務へ重複や旧経路を持ち込む場合は、その機能 PR で解消する。既存の対象内の問題や未判断を別 Issue へ送っただけで完了にしない。範囲の変更が必要なら理由と合意を計画・Issue に残す。

性能に影響する変更は関連する変更前測定を取り、機能追加と整理それぞれの対象コミット・条件を区別して比較する。最初の測定原本を残し、新機能のために数値が変わったことをリファクタリングの改善や悪化と混同しない。全体待ちを不要にする方針は、必要な tests・CI・review・実 OS / GUI 検証や性能基準を省略する許可ではない。

## 3. 既存作業との関係

| 既存対象 | 本計画での扱い |
|---|---|
| #23 | 性能測定の既存担当として再利用する。同目的の新Issueを作らず、変更前基準と最終比較を関連付ける。 |
| #292（引用バー）、#295（table delimiter） | 登録確認時点ではopen。RF2の該当移動とRF4/RF5は、受入済み修正のcurrent mainへの反映と証拠を確認してから進める。未反映時は重ならない範囲だけ扱い、計画の都合でmerge/再実装しない。 |
| #20/#21/#19 | caret点滅、新フォント方針、新spacing等の機能追加は今回へ混ぜない。既存geometry契約の整理と分ける。 |
| #22/#230 | 実OS/IMEの既知問題を現行証拠で確認する。基盤/既存不具合と新しい回帰を混同せず、未検証を成功にしない。 |
| #15/#18/#97/#236/#237 | 新しいMarkdown対応やsidebar操作など、既存の機能開発は別の目的として維持する。今回の全域完了のために無関係な機能を実装しない。 |

この表は開始時の整理であり、担当者は各Issueのcurrent bodyと状態を再確認する。既存不具合の修正が今回の契約成立を直接妨げる場合は、同じ原因の独立修正として明示し、機械的移動へ紛れ込ませない。

## 4. 計画要件から実行Issueへの対応

| 詳細計画の要件 | 実行担当 | 主な証拠 |
|---|---|---|
| 全領域棚卸し、既知問題と契約の分離、文書正本 | #298 | 対象台帳、構成別の現状結果、契約→テスト表、ADR照合 |
| 性能原本、測定条件、長時間/大量文書 | #23、#298、#314、#315 | 対象SHAとfixture付きraw data/集計、同条件比較 |
| 未使用性の立証、旧AADW/製品の不要物削除 | #299、#300 | 実行入口と依存関係、削除/維持根拠、現行入口の結果 |
| UI/presentation/Markdownの機械的分割 | #301、#302、#303 | 旧→新パス/シンボル表、公開API比較、同じ契約テスト |
| 入力先の正本、短い入力欄、本文IME/transaction | #304、#305 | 操作表、Unicode/範囲テスト、native入力証拠 |
| strict-matchとrebaseの区別、用途別projection、fallback | #306 | 経路図、snapshot契約表、viewport/stale/巨大blockテスト |
| geometry正本、正規化往復、描画/入力の整合 | #307 | deterministic座標テスト、実GUI/IME、入力/scroll比較 |
| cache無効化、局所更新、上限、jobと文書の寿命 | #368（RF5-B）、#309 | 所有者表、競合テスト、処理量とRSS/遅延 |
| 命名・保存・draft・操作identity・互換性 | #308、#309 | 状態所有表、遅延/失敗注入、実OS保存/復旧 |
| 現行GUI/CIの簡素化、権限/品質条件維持 | #310、#311 | 重複比較、入口/構成対応、代表run/artifact |
| 既存release/versionと依存/vendor | #312、#313 | 発行しないversionテスト、構成build、patch根拠/解除条件 |
| 実測最適化と移行残骸の撤去、最終受入 | #314、#315 | before/after、台帳全件判断、全要件の最終証拠対応 |

RF0の精査でパスや候補が変わった場合は、根拠と移管先をこの表・対象台帳・Issueへ記録する。関数名や候補の存在は実装担当が再確認する。本文で指定していない型名・ファイル名を強制しない。必要な対象を担当不在にしたり、未判断をfollow-upへ移して全体完了にしたりしない。

## 5. 担当者の開始・提出・完了手順

開始時には対象Issueと詳細計画の対応節、current code、関連ADR、先行Issueの証拠と現在の競合を読む。計画の仮説が現行実装と違う場合は、その根拠と変更範囲を先にPRへ示す。製品コードの実装・レビュー・GUI・次工程判断の役割は、default branchの [Commander Policy](aadw-command-policy.md) と [AGENTS.md](../AGENTS.md) に従い、この文書で別の判断規則を設けない。

各実装PRには次の内容を記載する。

| 提出欄 | 内容 |
|---|---|
| 対象 | Issue、作業ID、台帳候補ID、計画の対応節 |
| 原因と範囲 | current codeから確認した問題、変更する責務、変更しない挙動 |
| 構造 | before/afterの呼出関係・状態所有者・公開API・削除/維持根拠 |
| 検証 | 対象head、必要なbase context、コマンド/scenario、結果、ログ/run/artifact URL、未実施と理由 |
| 性能 | 影響がある場合の変更前基準・同条件比較・raw dataと集計 |
| 復旧 | revert単位と依存順、外部設定変更の有無、永続形式を変えていない確認 |
| 引き渡し | 満たした受入条件と証拠、残件、受け入れた後続境界、文書更新 |

Issueを閉じる前に、受入条件ごとに証拠を照合する。PRを作った、テストを移した、レビュー指摘が減ったというだけで完了にしない。必要な証拠が未実施/取得不能ならその範囲は未完である。維持が適切な候補は調査結果をもって維持と判断できるが、確認不能とは区別する。

## 6. 共通の検証範囲

Rust製品変更では、詳細計画にある次の基準を維持する。

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

通常feature/release成果物に影響する場合は次も確認する。

```sh
cargo test --workspace --locked
cargo build --release --locked -p hane
```

vendor/GPUI変更では、詳細計画第6節に記載したworkspace外の実字形・入力ソーステストを省略しない。Python/Shell/workflow変更はcurrent CIの該当テストと変更入口を確認する。native IME、geometry、filesystemの保証はunit成功だけでは代替しない。逆に製品へ影響しない文書/運用整理に全GUIを機械的に課さない。

本書保存時点でこれらは未実行である。性能基準は #23 を使用し、古い計測値やこの計画の予算値をcurrent実測として報告しない。実行やmergeの採否はcurrent Commander Policyに従う。

## 7. RF5-B — 実行Issue #368

当初はIssue作成APIに拒否され、本文だけをこの節に保持していた。2026年9月26日に [#368](https://github.com/hide212131/hane/issues/368) として登録した。以後は親 #297、RF8-A #314、最終受入 #315 と #368 の状態・証拠を対応付けて追跡する。

### タイトル

refactor(RF5-B): 表示キャッシュ・高さ索引・背景jobの更新責任と寿命を整理する

### 目的・依存

#301/#302 と #306 の当該境界、#309 の文書/session切替境界を受け入れた後に着手する。#307 と同じlayout状態を同時に変更しない。#23 の変更前基準が必要である。

### 作業

1. `block_cache`、`layout_cache`、`joined_parse_cache`、`line_owners`、HeightIndex/height_blocksとbackground jobについて、key・所有者・更新入口・無効化理由・容量/実行数上限・文書切替時処理を表にする。
2. source編集、disclosure、width/font/zoom、画像高さ、session切替を区別し、関連状態を整合して更新する小さなAPIに集約する。全cacheを汎用engineへ統合しない。
3. `JoinedParse` のstrict-matchとBlockIndexの条件付きrebaseを維持する。遅い結果が新しいjobのin-flightを消さないこと、旧文書のdetached jobも実行数に含めることを固定する。
4. 同期joinの行数/byte上限（計画基準では4,096行・256 KiB）と実行数管理をまず維持する。無関係なblockのcache/高さ測定を残し、局所編集で全体走査や過剰な再parse/shape/layoutを増やさない。

### 受入条件

- [ ] cache・索引・jobごとの責任者と無効化条件が明示され、複数箇所のばらばらな更新を整理している。
- [ ] 文書/フォルダ切替、遅い完了、新旧job競合、disclosure往復、split/joinを回帰テストしている。
- [ ] 巨大単一blockを同期全文処理せず、処理上限と文書切替をまたぐ実行数制限を維持する。
- [ ] parse/shape/layout回数と局所編集の処理量、入力/scroll/RSSを #23 の同条件基準と比較し、関連回帰を解消している。
- [ ] workspace tests/clippy、必要なfocused GUI、所有権表とbefore/after証拠を対象SHA付きで提出している。

### 対象外・PR・検証・復旧

閾値変更、cache algorithm最適化、session eviction、新runtimeは混ぜない。必要性が実測された変更はRF8で独立PRにする。cache種別または同じ無効化原因ごとにPRを分ける。

`cargo test --workspace --all-features`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`。巨大文書/小文書切替と画像遅延はfocused GUIも実施。各PRの戻し方と後続依存を明記し、計測対象の移動だけで改善としない。

## 8. 次に着手する作業

リファクタリングの最初の実装依頼は #298 とする。現行範囲・契約・検証条件を整え、#23 の変更前基準を確認する。その後は #299/#300 の根拠ある削除、#301〜#303 の機械的分割へ進む。文書/Issueの保存だけでは製品改修を開始していない。

全体の完了判断は #315 に集約する。RF5-B #368 の実施/証拠が未完のまま、親 #297 を完了にしてはならない。

## 9. RF0 初回調査記録（#298 の最初のPR）

### 9.1 調査の基準と読み方

2026年9月23日に、作業ツリーが空の `main` `f82ecef0a64b9a728c97aa9048db4606f399d549` を確認した。以下のコード・テスト・設定のパスは、特記しない限り[このコミット](https://github.com/hide212131/hane/tree/f82ecef0a64b9a728c97aa9048db4606f399d549)を指す。計画の調査基準 `ef9356c52bae06dee6cf49fa7b1a59ce004b52a0` との差分は18ファイルで、製品側では主に `crates/ui/src/view.rs`、`crates/ui/src/line.rs`、`crates/presentation/src/lib.rs`、表の区切り行に関係する索引・レイアウト・テストが変わった。#292 と #295 はこの `main` へmerge済み。調査時点で #298 にコメント・関連するopen PRはなく、open PR一覧も空だった。#298 と [#23](https://github.com/hide212131/hane/issues/23) はopenである。

現行の[AGENTS.md](../AGENTS.md)、[Commander Policy](aadw-command-policy.md)、[architecture](architecture.md)、関係するADR、manifest、CIを読んだ。旧 `/implement` は[CLAUDE.md](../CLAUDE.md)と現行workflowに照らして現行入口ではない。以下の「未判断」は削除可を意味しない。全ファイルの未使用性、実OSの振る舞い、性能をこの初回調査だけで証明していない。

### 9.2 対象台帳

各行の「根拠」は上記SHAのファイル、テスト名を指す。関連する進行中PRは調査時点では全行とも「なし」。変更前にmain/PRを再確認する。判定は今後の変更の提案であり、このPRでは削除・移動しない。

| ID・領域 / 対象 | 観測した事実、根拠、利用元・入口 | 判定、足りない証拠、守る条件と影響 | 担当・次の条件 |
|---|---|---|---|
| RF0-01 製品source / [`crates/document/src/lib.rs`](../crates/document/src/lib.rs) | `RopeBuffer` がsource byte・revision・範囲・anchorを所有する。editor/markdown/presentation/session/benchmarkがworkspace依存として利用。`unicode_edit_and_inverse_are_byte_based`、`source_bytes_stay_exact_across_a_bare_cr_edit`などを確認。 | **維持**。byte境界とCR/LF/CRLFの保持が契約。公開API縮小の可否は構成別呼出元とAPI差分が未確認。誤ると原文破損。 | #298→#300。削除/縮小前に全呼出元とsource契約を確認。 |
| RF0-02 編集 / [`crates/editor/src/lib.rs`](../crates/editor/src/lib.rs)、[`ime.rs`](../crates/editor/src/ime.rs)、`history.rs` | app/session/UIが `Editor` を使用。selection、IME、undo/redoとUTF-16変換を実装。`ime_contract.rs` は置換範囲・commit・undoをassert。 | **維持 / 責務分離候補**。入力先の決定と短い入力欄はUIにもある。native IMEの確定・取消、Unicode、transactionの実OS証拠は不足。誤編集の危険。 | #298→#304/#305。機械的分割前に操作表とnative IME確認。 |
| RF0-03 解析・索引 / [`crates/markdown/src/lib.rs`](../crates/markdown/src/lib.rs)、[`block_index.rs`](../crates/markdown/src/block_index.rs)、`list_editing.rs` | parserの `NodeKind`、局所/正式 `BlockIndex`、source-first list plannerを分担。presentation/UIが利用。`block_index_contract.rs` は古いparseの拒否と局所更新をassert。 | **責務分離候補**（大きい `lib.rs` のみ）。未使用APIは未認定。feature別呼出・巨大単一block・stale parseの処理量が未測定。誤判定は表示と入力位置へ波及。 | #298→#300/#303/#306。公開API・revision契約を固定してから分割。 |
| RF0-04 表示・geometry / [`crates/presentation/src/lib.rs`](../crates/presentation/src/lib.rs)、[`layout.rs`](../crates/presentation/src/layout.rs) | `SourceMap`、`VisualBlock`、`BlockLayout`をUIが利用。`layout_contract.rs` はsource↔point、表cell hit-test、区切り行の非表示と編集時の復帰をassert。 | **責務分離候補**（大きい `lib.rs` のみ）。#292/#295後の現行実装が基準。実字形・paint/入力の一致と画像遅延はunitだけで未確認。座標ずれの危険。 | #298→#302/#307。現行テストとfocused GUIを変更前条件へ。 |
| RF0-05 画面・入力 / [`crates/ui/src/view.rs`](../crates/ui/src/view.rs)、[`line.rs`](../crates/ui/src/line.rs)、`input.rs`、`actions.rs` | `EditorView` がsidebar、入力先、保存接続、background job、cache、描画を持つ。appが起動し、`view.rs` 内テストは選択・scroll・stale結果・draft等を扱う。`line.rs` の一部wrapperは `cfg(test)`。 | **責務分離候補**。行数だけで欠陥認定しない。同期join/旧job、layout・paintの寿命と実GUI証拠が不足。誤分割は入力・保存・描画へ波及。 | #298→#301/#304/#307/RF5-B。対象責務と状態所有者を限定してから移動。 |
| RF0-06 session・filesystem / [`crates/session/src/session.rs`](../crates/session/src/session.rs)、`service.rs`、`draft.rs`、`workfolder.rs`、`store.rs` | `DocumentSession` は世代、保存revision、ticketを区別し、`FileService` はI/O境界。`conflict_rules.rs`、`atx_editing.rs`、draft/store内テストが汚れた文書・保存・復旧を確認。UIが非同期結果を接続。 | **維持 / 責務分離候補**。実OSの権限・失敗注入・大量folder/session保持は未確認。誤ると未保存内容の喪失や外部変更の上書き。 | #298→#308/#309、保持量は#23。save/open境界のテストと実OS確認が先。 |
| RF0-07 実行・計測 / [`crates/app`](../crates/app)、[`crates/benchmark`](../crates/benchmark)、[`crates/metrics`](../crates/metrics) | appが通常起動、`instrument`/`timing-probe`はmanifestの追加feature。benchmarkは別binでbuffer/file open/layout等を測る。metricsは時間/RSSを集計。`gpui_baseline` exampleは`instrument`必須。 | **維持**。計測専用を製品の重複経路とみなさない。通常featureの単一スレッドテストは9.4節で確認済みだが、release成果物・起動/RSSのcurrent実測なし。 | #298/#23→#314。比較対象を変える前にrelease条件・fixture付き測定。 |
| RF0-08 tests・examples / [`crates/*/tests`](../crates)、`crates/app/examples/gpui_baseline.rs`、`vendor/gpui/examples` | workspace統合テストとcrate内unitがあり、vendorはworkspace外。通常/all-featuresでtest対象が変わる。 | **維持 / 未判断**。test専用参照やvendor exampleだけで未使用としない。9.4節のvendor対象テストとは別に、実OS/GUI・example・vendor全suiteの証拠が不足。 | #298/#300/#313。テスト削除前に守る条件と呼出構成を確認。 |
| RF0-09 現行運用入口 / [`.github/workflows`](../.github/workflows)、[`.github/scripts`](../.github/scripts) | `ci.yml`はPRでPython検査を実行し、Rust入力時にmacOS/Windows test+clippyを実行。`claude-fix.yml`は**PRコメント**のhandoff、GUIはPRコメント/手動dispatch、releaseも手動入口を持つ。 | **維持 / 統合候補**。ファイル名や旧AADW由来だけで廃止しない。workflowとrunner設定・権限・外部手動入口の調査が不足。誤削除は検証・権限境界を壊す。 | #298→#310/#311/#312。入口と権限を照合してから整理。 |
| RF0-10 旧final群 / `.github/scripts/final_pipeline.py`、`final_policy.py`、`final_fix_bridge.py`、`final_judge_probe.py`、`.github/tests/test_final_gate.py` | 現行workflow YAMLに4 script名の直接参照は見つからず、script相互と専用テストでimportされる。旧v1は履歴文書。 | **削除候補ではなく未判断**。手動CLI、外部runner、他scriptからの到達性は未確認。試行実行はしない。権限/通知/merge操作の危険。 | #298→#299。全入口・依存群・代替を示して初めて削除判断。 |
| RF0-11 ローカル・hosted手順 / [`scripts/`](../scripts) | `measure.sh`/`capture.sh`/`gui_validate.py`はREADMEとGUI文書から利用。hosted Python/Swift手順はworkflowや`aadw_gui_command.py`から参照され、一部は専用テストを持つ。 | **維持 / 統合候補**。`hosted_list_enter_gui.py`等の手動入口、Swift helper・runner依存は全件未確認。削除はGUI証拠取得を損なう。 | #298→#310。現行dispatch/CLI/外部runnerまで追って判断。 |
| RF0-12 設定・assets / [`assets/`](../assets)、`Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml` | work-folder SVGは`ui/icons.rs`から`include_bytes!`、app iconは`app/build.rs`とbundle/icon scripts、`phase4-feather.svg`はREADMEとapp既定文書で参照。manifestは9 crate、GPUI/pulldownはpath patch。 | **維持 / 未判断**。asset名だけで旧物としない。配布物内の相対画像解決、Windows icon・release設定の実物確認は不足。 | #298→#300/#312/#313。通常/release/OS別の利用証拠が先。 |
| RF0-13 文書・ADR / [`docs/`](../docs)、[`README.md`](../README.md)、[`AGENTS.md`](../AGENTS.md) | 計画正本は本書と詳細計画。旧R0〜R5は`docs/history/refactor-cycle-1/`へ保存。architectureとADR-0003/0004/0005/0019/0021/0022/0025/0027/0029等が契約を記す。 | **維持 / 文書照合候補**。過去の「完了」やADRの記載状態をcurrent検証結果へ読み替えない。全ADRの実装/証拠照合は未完。 | #298→#315。文書差を根拠付きで更新し、原本を保持。 |
| RF0-14 vendor GPUI / [`vendor/gpui/HANE-PATCH.md`](../vendor/gpui/HANE-PATCH.md)とmacOS実装 | workspace外のpatchは合成斜体 #101 と入力ソース切替後IME再同期 #126。前者は実字形、後者はnative入力が関係し、CIには対象vendorテストがある。 | **維持**。workspace成功はvendor単体成功を含まない。Mac実フォント・日本語IMEのGUI、上流同等修正が未確認。 | #298→#313。patch解除前にvendorテストとfocused GUI。 |
| RF0-15 vendor pulldown-cmark / [`vendor/pulldown-cmark/HANE-PATCH.md`](../vendor/pulldown-cmark/HANE-PATCH.md) | ATX空白、code span CRLF、hard breakの限定patch。workspaceの`atx_conformance.rs`/`line_breaks.rs`とvendorの`tests/errors.rs`が関連。 | **維持**。`tests/errors.rs`は9.4節の隔離コピーで成功。上流版との同等性とvendor全suiteは未確認。削除はMarkdown原文とrangeへ影響。 | #298→#313。上流確認と両側の契約テスト後に判断。 |

### 9.3 保護する条件とテストの対応

下表の「確認」はassert内容を読んだことを指す。実行結果は次節に分けた。`MemoryFileService`、`FixedAdvanceShaper`、`gpui::TestAppContext`による成功は、実OSの入力・字体・画面・ファイル操作の代わりにはならない。

| 条件・実装 | 既存テストで確認したassert | 不足・変更前の担当 |
|---|---|---|
| source byteと編集結果：`RopeBuffer`、`Editor`、`FileService` | `document::unicode_edit_and_inverse_are_byte_based`はbyte単位の逆編集、`session/atx_editing::heading_drag_selection_ime_and_save_reopen_preserve_spelling`はタブ・emojiを含む原文を保存/再読込して同一文字列と見出し種別をassert。 | 実OSでCRLF・非ASCII名の保存、失敗時の原本/一時ファイル確認は別。#298/#309、保存経路変更前。 |
| selection、undo/redo：`editor::Selection`/`History` | `typing_and_removing_atx_markers_reparses_through_undo_and_redo`は入力・Undo/Redoごとのsourceと表示、`ime_marked_range_selection_commit_and_undo_share_one_source_contract`はUndo後のsourceと選択範囲をassert。 | GUIのdrag→編集→Undo/Redoと再読込を同一fixtureで確認。#298/#304/#307、入力分割前。 |
| IME・Unicode境界：`editor::ImeState`、`ui::EditorView`、GPUI patch | `ime_contract.rs`はUTF-8↔UTF-16往復、リスト行への一回commit、構造prefix非注入をassert。`view.rs`にinline rename IME testがある。 | 日本語入力ソース切替後、通常段落・リスト・renameで確定/取消とsource offsetを実macOS/Windowsで観測。#298/#304/#305/#313、native入力変更前。 |
| source↔表示↔座標：`SourceMap`、`BlockLayout`、`EditorView` | `source_map_contract::every_editable_source_boundary_round_trips_when_its_construct_is_disclosed`は編集可能byte境界の往復、`layout_contract::every_editable_source_offset_round_trips_through_the_layout`は位置往復、表cell/区切り行テストはhit-test/非表示行をassert。 | 実フォント・zoom・soft wrap・表・画像読み込み後にpaint座標とclick/dragの編集位置を照合。#298/#307、geometry変更前。 |
| revision、文書世代、古い結果：`BlockIndexState`、`DocumentSession`、`EditorView` | `block_index_contract::a_stale_parse_never_replaces_what_is_already_published`は古いparse拒否、`conflict_rules::opening_a_clean_session_reuses_it_and_invalidates_derived_state`は世代変化、`view.rs`の`a_stale_new_session_read_from_a_replaced_work_folder_is_discarded`は旧folder結果拒否を確認。 | 遅延順を入れ替えた保存・parse・画像結果と文書切替を同時に試すfocused testが必要。#298/#306/#309/RF5-B、寿命変更前。 |
| 未保存内容・draft・外部変更：`DocumentSession`/`FileService`/`DraftStore` | `conflict_rules::opening_a_file_into_a_dirty_session_is_refused`は上書き拒否、`a_save_refuses_to_clobber_an_external_edit_until_the_user_says_so`は外部編集保護、`draft::a_written_draft_round_trips_through_recover`は復旧内容、UI testは復旧失敗の表示を確認。 | 実OSで書込拒否/ディスク不足/再起動を挟む「入力→draft→復旧→保存」を確認。#298/#309、保存・draft整理前。 |
| 実行権限・対象SHA：`AGENTS.md`、workflow、CI path filter | `.github/tests/test_aadw_gui_command.py`はGUI routing、`test_codex_usage_limit_fallback.py`はworker credential分離、`test_rust_ci_path_filter.py`はRust job選択をassert。現行`claude-fix.yml`はPRのtrusted actorとexact headを照合する。 | 実際のworkflow run、権限、base contextを操作ごとに確認。旧final群のテストは現行入口成功の証拠にしない。#298/#299/#310/#311、運用整理前。 |
| 処理上限・性能：`BlockIndex`、UI joined parse/cache、benchmark | `block_index_contract::local_editing_costs_the_same_in_a_small_and_a_large_document`は局所更新量、`view.rs`の`joined_parse_work_is_bounded_across_scrolling_and_document_switches`は同期処理上限、`layout_cache_key_rejects_each_geometry_input_independently`はcache keyをassert。 | current mainのrelease起動、入力/scroll/RSS、巨大単一段落と長時間session保持の実測なし。#23、該当性能経路の変更前。 |

追加するテストはこのPRに混ぜない。優先する具体例は、(1) `- item`末尾に日本語IMEの未確定文字を置き、入力ソースを切替後に確定・取消し、sourceにprefixが重複せず一回だけ挿入/復元すること（#304/#313）、(2) dirty文書Aの保存要求中にBへ切替え、Aへの遅い成功/失敗と古いparseを逆順で返し、Bの本文/保存表示/draftを変えないこと（#309/RF5-B）、(3) 表の非表示区切り行を挟むsoft wrapのclick/dragを実字形・zoomで行い、source範囲とpaint位置を一致させること（#307）である。望ましい結果と既存不具合を混ぜない。

### 9.4 変更前の検証と残件

検証対象は上記 `f82ecef0a64b9a728c97aa9048db4606f399d549`、ローカルはmacOS 26.6.2 (25G83) / Darwin 25.6.0 arm64、Rust/Cargo 1.93.1（Homebrew）、Python 3.14.3。[保存した検証証拠とコマンド・終了コード・除外情報の一覧](refactor-evidence/rf0-issue-298/README.md)を参照。2026年9月23日の元ログは作業環境の `work/rf0-logs/` にあり、リンク先は個人の絶対パスを除いた静的コピーである。この補完でテストを再実行していない。ローカル実行結果はGitHub CIと区別する。

| 対象・コマンド | 結果と証拠 / 適用範囲 |
|---|---|
| `cargo test --workspace --all-features` | **失敗、exit 101**。[全workspaceログ](refactor-evidence/rf0-issue-298/logs/cargo-test-all-features.log)では`hane-ui --lib`の199件が完走する前にSIGABRT。別実行の`cargo test -p hane-ui --lib --all-features`も**exit 101**で[再現](refactor-evidence/rf0-issue-298/logs/hane-ui-parallel-repeat.log)。対応するmacOS[最初のcrash抜粋](refactor-evidence/rf0-issue-298/crash/workspace-all-features.json)と[再現時の抜粋](refactor-evidence/rf0-issue-298/crash/ui-parallel-repeat.json)はHIToolboxの入力ソースAPI同時呼出、GPUI→`EditorView::from_sessions`のstackを記録。assert失敗ではなく、テスト並列性と製品動作の切り分けは未完。#298で追う。 |
| `cargo test -p hane-ui --lib --all-features -- --test-threads=1` | **成功、exit 0、199件**。[UI単体ログ](refactor-evidence/rf0-issue-298/logs/hane-ui-serial.log)。単一スレッド条件であり、並列SIGABRTの解消とは扱わない。 |
| `cargo test --workspace --all-features -- --test-threads=1` | **成功、exit 0**。[all-features単一スレッドログ](refactor-evidence/rf0-issue-298/logs/cargo-test-all-features-serial.log)にworkspace全suiteとdoc tests。 |
| `cargo test --workspace --locked -- --test-threads=1` | **成功、exit 0**。[通常feature単一スレッドログ](refactor-evidence/rf0-issue-298/logs/cargo-test-default-serial.log)にworkspace全suiteとdoc tests。 |
| CIのPython 6入口（`ci.yml`記載順） | ローカル実行は各**exit 0**。[path filter](refactor-evidence/rf0-issue-298/logs/test_rust_ci_path_filter.log)、[GUI command](refactor-evidence/rf0-issue-298/logs/test_aadw_gui_command.log)、[code block手順](refactor-evidence/rf0-issue-298/logs/test_hosted_code_block_gui.log)、[Claude診断](refactor-evidence/rf0-issue-298/logs/test_claude_failure_diagnostic.log)、[Codex fallback](refactor-evidence/rf0-issue-298/logs/test_codex_usage_limit_fallback.log)、[release version](refactor-evidence/rf0-issue-298/logs/test_release_version.log)。Codex fallbackの元ログは0 bytesで成功出力はない。コマンド全文と独立した[PR CIのPython job](https://github.com/hide212131/hane/actions/runs/35848315415/job/107139720924)は証拠一覧を参照。 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | **成功、exit 0**。[clippyログ](refactor-evidence/rf0-issue-298/logs/cargo-clippy-all-features.log)。macOS all-targets/all-featuresの静的検査であり、Windows実行結果ではない。 |
| vendor GPUI: macOS text system testと入力ソース切替test（コマンド全文は証拠一覧） | **成功、各exit 0**。[text system 7件](refactor-evidence/rf0-issue-298/logs/vendor-gpui-text-system.log)、[入力ソース切替1件](refactor-evidence/rf0-issue-298/logs/vendor-gpui-ime-source.log)。実アプリでの日本語IME操作は別。 |
| vendor pulldown-cmark: repo内の`--test errors`と隔離コピーでの同test（コマンド全文は証拠一覧） | repo内直接実行はCargoのworkspace構成によりテスト起動前に**exit 101**。[直接実行ログ](refactor-evidence/rf0-issue-298/logs/vendor-pulldown-errors.log)。同じvendor内容のrepo外コピーでは**exit 0、25件**。[隔離実行ログ](refactor-evidence/rf0-issue-298/logs/vendor-pulldown-isolated-errors.log)。両結果を同一視しない。 |
| release build、Windows実機/CI、実GUI・実IME、#23の性能測定 | **未実施、ログなし**。[証拠一覧の不足欄](refactor-evidence/rf0-issue-298/README.md#除外した情報と範囲)と[#23](https://github.com/hide212131/hane/issues/23)を参照。macOS単体テストからこれらを推定しない。担当は#298/#23/#313、変更領域で必要になる前。文書だけの本PRではGUI全件を提出条件にしない。 |

`ci.yml`のRust jobは文書だけのPRではpath filterによりskipされ得る。[PR #319の文書CI run](https://github.com/hide212131/hane/actions/runs/35848315415)はhead `2eeb94d92c85c99259c3becfab9902b79a34035d`、base `f82ecef0a64b9a728c97aa9048db4606f399d549`を対象にPython/path filterが成功し、macOS/WindowsのRust jobは**skip**された。この補完後のheadに対するCIは別のrunとして確認する。CI全体の緑をRust成功へ読み替えず、この表の失敗や未実施を変更しない。

### 9.5 #23へ渡す性能条件と限定引き渡し

[`docs/baseline/README.md`](baseline/README.md)は2026年8月26日の旧R0測定原本で、製品基準commitは`a811425`。warm startup、10/100 MB RSS等の数値をこの `main` の結果とは呼ばない。現行の`hane-bench`はbuffer/file open/layout等の非GUI計測入口であり、#23が求めるprocess起動・work folder・訪問済みsessionの実測原本を代替しない。#23へ、release buildと静穏な同一機種/OS/電源/refresh rate/profileを記録し、fixtureと試行数を添えて、空/小/100 MB起動、空/1/10/100 MB RSS、巨大単一段落の入力/scroll、1k/10k件folder、10/100/1,000件訪問済みsession、長時間のfolder/文書切替・編集・画像表示を測る作業を残す。少なくとも起動は可能なら30回、相対10%/絶対gateを同条件で比較し、超過時は独立再測定する。計測に影響する#306/#307/RF5-B/#309/#314等より前に該当する変更前値を採る。

このPRから渡せるのは、対象の所有関係、既存assert、未実施の境界を追える**調査記録**だけである。独立した文書照合や旧入口の読み取り調査は続けられる。削除、製品の責務移動、geometry/保存/cacheの意味変更、性能改善の着手可判定はまだ渡さない。#298全体の契約テスト・構成別検証・#23実測、ADR全件照合は残る。#299や#301以降の実装、#23/#298/#297のcloseは本記録の完了から自動で導かない。


### 9.6 RF1-A: 旧 final 経路の到達性再確認（#299）

2026年9月26日、main `94a15eff5fbe694a85e60674a0075a0280d2d40c` を基準に RF0-10 を再確認した。現行の `.github/workflows/` には final judge / final gate workflow がなく、`ci.yml`、`claude-fix.yml`、`codex-usage-limit-fallback.yml` を含む現行workflowから `final_pipeline.py`、`final_policy.py`、`final_fix_bridge.py`、`final_judge_probe.py` への参照はない。repo内の直接importはこの4本相互と専用 `.github/tests/test_final_gate.py` に閉じていた。現行文書の実行入口は Commander Policy と AADW v2 workflowであり、旧 final judge は `docs/history/aadw-v1/` に履歴として保持されている。

このため #299 の最初の削除単位として、上記4 scriptと専用testだけを撤去する。現行経路が共有する `pipeline_api.py`、`gui_policy.py`、`claude_fix_state.py`、`aadw_notify.py`、observer/reconcile群は削除しない。これらには旧 `hane/final-judge` statusを履歴として認識する処理が残るが、旧scriptへの実行入口ではなく、現行機能と共有するため本削除単位の対象外とする。

外部から旧scriptを直接起動するcurrentの文書化された手順・workflow入口はrepo内にない。self-hosted runnerのcurrent workflow入口はCodex usage-limit fallbackであり、旧 final 群を参照しない。削除後は current CI のPython検査を通し、検索で旧scriptの実行参照が履歴文書と本記録以外に残っていないことを確認する。問題時はこの削除PRをrevertし、旧経路を個別に再作成しない。
