# Hane 全域リファクタリング計画 — 第2サイクル

調査日: 2026年9月23日  
対象: `hide212131/hane`  
調査基準: `main` の `ef9356c52bae06dee6cf49fa7b1a59ce004b52a0`

## 1. 結論と計画の位置付け

全リポジトリを対象にするが、一括で書き直さない。不要物の除去、責務の分離、判断・状態の重複解消、計測に基づく最適化を、それぞれ検証可能なPRに分けて進める。

目標は、単に行数やファイルサイズを減らすことではない。「同じ仕様を複数箇所で判断する」「関連する状態を複数の所有者が更新する」「一つの修正のために別経路も追いかけて修正する」という構造を減らすことである。

既存の `docs/refactor-execution-plan.md` は、2026年8月29日の更新で旧R0〜R5を完了としている。今回の計画はそのやり直しではなく、現在の実装を起点にした第2サイクルとする。既に導入済みの `BlockIndex`、`DocumentSession`、`FileService`、ブロック仮想化などを再設計・再実装することを前提にしない。[S1] [S2]

今回の調査は、対象SHAの主要実装、設計文書、ワークフロー、進行中PRの静的確認である。全ファイルの精査、未使用性の完全な証明、テスト実行、GUI検証、性能測定は実施していない。以下では確認済みの構造と、実施時に立証する削除・最適化候補を区別する。ローカルの未コミット変更は調査対象に含まれない。

## 2. 現状から見た優先順位

| 対象 | 確認できた事実 | 計画上の扱い |
|---|---|---|
| `crates/ui/src/view.rs` | 563,504 bytes。入力、サイドバー、ファイル操作の調停、非同期処理、表示キャッシュなどの状態を所有する。 | 最優先で責務を切り分ける。ただし単なるファイル分割で完了にはしない。 |
| `crates/presentation/src/lib.rs` | 281,063 bytes。位置対応、表示型、表示方針などを含む。 | 位置対応・表示方針・projectionなどの境界を明確にする。 |
| `crates/markdown/src/lib.rs` | 121,646 bytes。構文型、解析、projection関連の実装を含む。 | 意味解析の正本を保ったまま、公開APIと内部責務を整理する。 |
| `crates/ui/src/actions.rs` | 多くのキー操作で、フィルタ・rename・本文の入力先分岐を繰り返している。 | 入力先決定と対象固有動作を分離する。 |
| `.github/scripts/` | 旧final judge関連のスクリプトとテストが残っている。 | 現行入口からの到達性を調べ、未使用が証明できた単位で削除する。 |
| 設計・測定資料 | 旧リファクタリングは完了扱いだが、現行の起動・メモリ再測定を扱うIssue #23はopen。 | 計画・現行設計・性能原本を整合させる。 |

サイズはGitのblobサイズであり、コメントや同居テストを含む。無駄なコードの量や製品LOCを表してはいない。[S3] [S4] [S5] [S6] [S7] [S22] [S23]

### 削除対象と混同してはいけないもの

`EditorView` にあるrevision、document identity、work-folder generation、保存ticket、非同期job identityは、それぞれ異なる寿命や競合を扱っている。これらを「似たカウンタだから」という理由で一つにまとめない。

また、`JoinedParse` のstrict-match検証と、`BlockIndexState` の安全に条件を満たした場合のrebaseは異なる契約である。表示用の `ListProjection` と同期編集用の `ListEditProjection` も目的が異なる。暫定解析、巨大ブロックのバックグラウンド処理、sourceを失わないfallbackは、削除に先立って同等の安全性と性能を示す必要がある。[S8] [S9] [S10]

## 3. 対象範囲と非目標

対象は、製品コード、テスト、examples、計測コード、scripts、GitHub Actions、設定、assets、設計・運用文書、vendorのHane固有パッチである。全領域を棚卸しするが、問題がない領域まで変更しない。

既存の9-crate構成と、`document` / `editor` / `markdown` / `presentation` / `session` / `ui` / `app` の依存方向を原則として維持する。`benchmark` / `metrics` も既存の目的を起点に見直す。[S2] [S11]

今回のリファクタリングには、新しいMarkdown構文対応、サイドバー新機能、全面的なUI変更、汎用プラグイン基盤、新しい非同期ランタイム、独自イベントバス、DIフレームワーク、AADW用の状態DBを含めない。依存ライブラリの大型更新も、必要性が確認できた場合の独立変更とする。

永続化形式を変えない。Markdown本文、設定、draftについて既存の利用データを読める状態を維持する。内部整理のために既存文書を一括変換・正規化しない。

## 4. 完成後の責務配置

| 層 | 一か所に集約する責務 | 持たせない責務 |
|---|---|---|
| `document` | source、位置単位、revision、編集差分 | Markdownの意味やGPUI |
| `editor` | 汎用編集、selection、IME、undo/redo transaction | リストなどの構文固有ルール |
| `markdown` | 構文解析、sourceに基づく編集計画、用途別projection | pixel座標やUIの見た目 |
| `presentation` | source↔visual、表示方針、layoutと座標計算 | ファイルI/O、アプリ操作の判断 |
| `session` | session/file identity、保存・draft・renameに関する状態と判断 | GPUIの描画やイベントループ |
| `ui` | 入力の接続、非同期要求の実行、表示、画面状態 | sourceにない文書構造の捏造、保存規則の別実装 |
| `app` | 起動と依存オブジェクトの組み立て | 編集・描画・保存ロジック |

`ui` 内部は、サイドバー、短い入力欄、本文入力、viewport、render、非同期要求の接続などへ整理する。`presentation` は位置対応、表示方針、projection、layout、高さ索引を見分けられる配置にする。

これらは責務の境界案であり、最終的な型名・ファイル名の固定指示ではない。実装担当はcurrent codeと呼び出し関係から最小の分割を選ぶ。小さな具象structや既存moduleで足りるなら、新crateや汎用trait群を作らない。

## 5. フェーズ全体

| フェーズ | 目的 | 主な完了条件 |
|---|---|---|
| RF0 | 現状・契約・測定原本を固定する | 対象、既知問題、テスト、性能比較条件が明確になる。 |
| RF1 | 不要物と旧実行経路を除去する | 削除根拠があり、現行入口と参照が整合する。 |
| RF2 | 巨大ファイルを責務ごとに分ける | 振る舞いを変えず、後続の責務移譲が小さな差分で行える。 |
| RF3 | 入力先と編集処理を整理する | 入力先決定が集約され、IMEとtransactionの契約を保つ。 |
| RF4 | 解析とprojectionの契約を整理する | 同一snapshotを使うべき経路が一致し、目的の違うprojectionは混同しない。 |
| RF5 | layout・描画・キャッシュ管理を整理する | 座標の正本と無効化責任が明確になり、処理量の上限を保つ。 |
| RF6 | 保存・draft・renameの調停を整理する | 状態の所有者が明確になり、I/Oの失敗や遅延で文書を失わない。 |
| RF7 | 現行の開発・検証・リリース基盤を簡素化する | 重複は減るが、権限境界と検証・リリース条件は維持する。 |
| RF8 | 計測で示された無駄を最適化し、全域を受け入れる | 正しさ・性能・保守性の証拠が揃い、移行用コードが残らない。 |

番号は旧計画のR0〜R5と区別するためのものである。

## RF0 — 現状と変更してはいけない契約を固定する

### 作業

**棚卸しを作る。** 各候補について、対象パス、問題の根拠、呼び出し元、削除／統合／責務分離／維持／未判断、危険性、回帰テスト、依存するPR、完了条件を記録する。リストは既存の実施計画で管理し、専用管理システムを追加しない。

未使用性はテキスト検索だけで判断しない。Rustのfeature・platform条件、macroやasset参照、Python/Shellからの呼び出し、workflow_dispatch、文書化されたCLI、runner上の外部設定も確認する。テストしか呼ばないことは調査の入口にはなるが、それだけで削除の根拠にはしない。

**既存テストと不具合を分ける。** 自動テスト、GUI、性能測定の実行条件と結果をcurrent SHAに結び付ける。既存の失敗は再現条件と影響を記録し、望ましい仕様として固定しない。データ損失・誤編集などの問題が出た場合は、構造変更に紛れ込ませず、原因ごとに修正する。

**性能の原本を更新する。** Issue #23を利用して、起動、空・1/10/100 MB文書、長大な単一段落、1k/10k件のwork folder、訪問済みsession増加、長時間の切替・編集を測定する。機種、OS、電源、refresh rate、profile、fixture、サンプル数を記録する。旧レポートの数値をcurrentの実測として扱わない。[S7]

**文書の正本を整える。** `docs/refactor-plan.md` は目標・契約・範囲、`docs/refactor-execution-plan.md` は実施順と進捗とする。完了した旧サイクルの記録を区別し、第三の競合する恒久計画を増やさない。ADRは記載状態と実装・検証証拠を照合する。実装があるだけで「提案」を自動的に「検証済み」へ変更しない。

### PRの単位と完了条件

棚卸し・文書整理と、追加する契約テスト・測定結果は差分を分けられる。削除やAPI変更はまだ混ぜない。

対象範囲、保護する振る舞い、既知の未完事項、測定条件が明確になり、後続の変更を同じ物差しで判定できれば完了とする。製品に影響しない独立した整理に、不要な全GUI検証を毎回要求しない。

## RF1 — 使われないコードと旧実行経路を除去する

### 作業

**AADW旧経路を到達性で判定する。** 最初の調査候補は、`.github/scripts/final_pipeline.py`、`final_policy.py`、`final_fix_bridge.py`、`final_judge_probe.py`、対応テストである。旧final pipelineの参照は旧設計資料、関連スクリプト、テストで確認できるが、この時点では未使用を確定していない。[S6]

現行workflow、CLI、import、手動実行手順、runner連携まで確認して、使用していない実行経路をひとまとまりで削除する。現行機能が再利用する部品は巻き込まない。まだ入口が動いている場合は、代替・停止・確認・削除の順に移す。旧経路そのものを検査するテストは、本体とともに削除できるが、現行契約を守るテストは残す。

**製品側の未使用候補を精査する。** 呼ばれない公開API、互換alias、終了済み実験、設定、fixture、assetを確認する。platform限定コード、feature限定コード、安全なraw-source fallback、計測に使うコードを一律に消さない。

`ui/line.rs` の `presented_block` 系の一部wrapperは `#[cfg(test)]` であり、製品の複数実行経路と数えない。テストの読みやすさに役立つなら残してよい。[S12]

### PRの単位と完了条件

一つの廃止機能・到達不能な依存群を一つのPR単位とする。関係のない製品コード削除とworkflow整理を同居させない。

削除した各対象に根拠があり、現行入口・設定・文書に切れた参照がなく、関連するbuild/testが通れば完了とする。「検索で出なかった」だけでは完了にしない。復旧は削除PRのrevertで可能な状態にする。

## RF2 — 巨大ファイルを機械的に分割する

### 作業

**`view.rs` を責務別の配置へ移す。** サイドバー表示、フィルタ・rename、scroll/zoom、保存要求の接続、background parse、cache、描画の組み立て、テストを見分けられるmoduleへ移す。この段階では処理順や条件判定を変えない。

**`presentation/lib.rs` と `markdown/lib.rs` を整理する。** 前者はsource map、表示型と表示方針、projection、高さ索引、layoutとの境界を分ける。後者は構文型、parser、marker導出、用途別projection、編集plannerの境界を明確にする。既存の `block_index.rs`、`block_store.rs`、`list_editing.rs` を無意味に再分割しない。

**公開APIを維持する。** まず既存の外部呼び出しが通るように移動し、不要な公開範囲の縮小は別の差分で行う。分割の都合でフィールドや関数を広範囲に `pub` 化しない。大量の整形変更、命名変更、アルゴリズム変更を同時に行わない。

### PRの単位と完了条件

一つの責務の移動を一つのPR単位とする。テストは責務別に移すが、回帰ケースを減らさない。

既存の外部動作が変わらず、移動と意味変更をレビューで区別できれば、このフェーズは完了とする。ただし `impl EditorView` を複数ファイルに移しただけでは全体リファクタリングは完了しない。状態の所有権と更新APIの改善はRF3〜RF6で行う。

## RF3 — 入力先の決定と編集処理を整理する

### 作業

**入力先を一度だけ決める。** `actions.rs` で繰り返している、sidebar filter → inline rename → documentという分岐を、入力先の解決と対象別dispatchへ整理する。例えば小さな `InputTarget` enumを用いるが、キー操作の用途差を消すことを目的にはしない。[S5]

**短い入力欄の重複を除く。** フィルタとrenameで共通する文字列、selection、grapheme境界移動、range replacement、UTF-16変換などを小さな具象モデルにまとめる。renameの確定・取消・I/O待ちと、フィルタの検索条件更新は対象固有処理として残す。本文エディタ全体を両者へ組み込んだり、第二の汎用IME基盤を作ったりしない。

**本文編集の契約を守る。** selection、IME、undo/redoは既存Editorの責任を維持する。リスト編集はADR-0029のsource-firstを維持し、Markdown層が編集計画を返し、Editorが汎用range replacementをrecorded transactionとして適用する。入力途中のリスト構造をViewだけに持たせる旧方式を復活させない。[S10]

**挙動変更を混ぜない。** sidebar treeの新しいキーボード操作、複数selectionのリスト編集、新しいショートカットなどはこのフェーズの目的に含めない。

### 検証と完了条件

本文・フィルタ・renameそれぞれで、文字入力、左右移動、選択、削除、clipboard、Enter/Escape、focus切替、IME commit/cancelを確認する。日本語、絵文字、結合文字、UTF-8とUTF-16の境界を含める。リストでは一操作が期待する一transactionとなり、undo/redoでsourceとselectionが戻ることを確認する。

ネイティブIMEの正しさはunit testだけでは判定せず、変更が影響するmacOS/Windowsの実入力経路でfocused GUI検証を行う。入力先決定の正本が一か所になり、同じ操作が入力欄間で不整合に実装されず、既存の対象別仕様が維持されれば完了とする。

## RF4 — Markdown解析とprojectionの契約を整理する

### 作業

**各データの目的と鮮度を明文化する。**

| データ | 主な目的 | 維持する契約 |
|---|---|---|
| `BlockIndexState` | sourceのブロック境界と正式・暫定状態 | 条件を満たすrebaseとpublish優先順位を維持する。 |
| `JoinedParse` | 複数物理行にまたがる意味解析snapshot | revision・block identity・rangeの厳密一致を維持する。 |
| `ListProjection` | 表示用のリスト構造 | 表示の都合を編集の唯一の根拠にしない。 |
| `ListEditProjection` | current sourceに対する同期編集判断 | 背景の正式解析待ちで編集を止めない。 |

これらは異なる目的の派生データであり、型が似ているという理由だけでは統合しない。[S9] [S10]

**同じ意味計算の重複を減らす。** 同一snapshotについて、通常描画、1行だけの描画、上下移動、隣接block移動、hit test、marker disclosureが別の意味解析へ分岐していないか調べる。同じsnapshotを共有すべき経路は共通projectionを使う。viewportの大小によってMarkdownの意味が変わらない構造にする。[S9]

**引数の増殖を止める。** `presented_block_with_table_projection` のような、多数のprojectionと表示条件を受け取るAPIを確認する。関連入力を具象のrequest/contextへまとめ、互換wrapperを不要になった時点で撤去する。ただし、巨大で内容不明な「何でもcontext」に置き換えない。snapshot identityやrevision検証を省略しない。[S12]

**fallbackを役割別に整理する。** 未対応構文のsource保持、正式解析待ちの暫定表示、入力途中の編集planner用回復処理を区別する。異なる保証を持つfallbackを一つの曖昧な成功経路にしない。

### 検証と完了条件

複数行の強調・コード・引用・リスト、画面外の閉じmarker、同一blockを異なるviewportで見る場合、遅れて届く結果、正式結果と暫定結果の競合を検証する。行数が少なくbyte数が巨大なblockも必須とする。

意味解析の正本、projectionごとの入力・出力・寿命が明確になり、同条件の表示・navigationの意味が一致すれば完了とする。すべての解析を同期化して「経路を一本化」する変更は認めない。

## RF5 — layout・描画・キャッシュの責任を整理する

### 作業

**座標の正本を維持する。** `BlockLayout` を中心に、描画、hit test、caret、selection、IME候補位置、上下移動が同じgeometryを使うように接続を整理する。UIで独自の幅・高さ補正を増やさない。表・引用などの表示修正が異なる経路で食い違わないよう、契約テストを共用する。[S2]

**キャッシュごとに責任者を決める。** `block_cache`、`layout_cache`、`joined_parse_cache`、`line_owners`、高さ索引について、key、所有者、更新入口、無効化条件、上限、文書切替時の破棄を明示する。すべてを汎用cache engineへ統合するのではなく、関連する状態をまとめて更新する小さなAPIにする。[S8]

source編集、selection/IMEによるdisclosure変更、width/font/zoom変更、画像の高さ確定、session切替を区別する。caret位置が変わってdisclosureも変わる場合はlayout変化が必要になり得る。一方、同じsource・style・disclosure・width・fontでcaretの表示だけが変わる場合は、文字geometryを変えない。

**上限と寿命を保つ。** 現在の同期join判定は4,096行と256 KiBの両方を使い、background joined parseは文書切替をまたいだ実行数も管理している。閾値はまず維持し、変更する場合は独立した計測と根拠を示す。古い結果が新しいjobのin-flight状態を消さないことも維持する。[S8] [S12]

**SourceMapの検証を正しく定義する。** hidden/synthesized要素があるため、source↔visualを無条件の一対一対応として検査しない。Bias/affinityを含む正規化された往復と、編集可能な境界の契約を確認する。

### 進行中PRとの関係

調査時点では、引用バーのPR #292とtable delimiterのPR #295がopenである。関連するlayout・描画の整理は、これらの修正が受け入れられた基準へ追従してから行う。計画の都合だけでmergeしたり、同じ修正を新しいPRで再実装したりしない。無関係な入力欄・運用整理まで止める必要はない。[S13] [S14]

### 検証と完了条件

可変行高、soft wrap、tableセル、非表示delimiter、入れ子quote、zoom、画像読み込み、選択中のscroll、文書切替を確認する。計測が必要な区間では再parse、再shape、再layoutの回数も観測する。

geometryの正本、cache無効化、stale結果の扱いが一貫し、局所編集の処理量が不要に文書全体へ広がらなければ完了とする。

## RF6 — session・保存・draft・renameの調停を整理する

### 作業

**既存のI/O境界を利用する。** `session` には既に `service.rs`、`session.rs`、`draft.rs`、`identity.rs`、`naming.rs`、`store.rs`、`workfolder.rs` がある。新しいFileServiceを作るのではなく、Viewに残る判断と状態を適切な所有者へ移す。[S15]

`FileService` は同期traitであり、呼び出し側が入力経路外のexecutorで動かす設計になっている。この境界を保ち、整理のためだけにasync traitや新しいruntimeを導入しない。GPUIのtask起動はadapterに残し、要求を作る判断と結果を受け入れる判断をUI非依存で検査できる形にする。[S16]

**セッション単位の関連状態をまとめる。** Viewの `title_sync_pending`、`title_sync_in_flight`、`title_sync_scheduled`、`title_sync_deferred` などを対象に、関連状態と更新操作をセッション単位の小さな型へまとめる。ただし、独立して同時に成立する状態まで一つの排他的enumへ押し込めない。[S8]

**操作identityを維持する。** loading path、latest open target、work-folder generation、document revision、SaveTicketを混同しない。フォルダ切替後の遅いread、rename中のautosave、古いH1 debounce、終了時draft flushが現在の文書・パスへ誤適用されないようにする。

**保持方針は計測して決める。** 訪問済みsessionの保持を明文化する。evictionはIssue #23の測定で実害が確認された場合に限り検討し、導入する場合もdirty/IME/save中の扱いを先に契約化する。単にメモリ表示を小さくするための複雑な退避機構は追加しない。[S7]

### 検証と完了条件

保存失敗、外部変更、保存の連続要求、保存とrenameの競合、名前衝突、work-folder切替、draft復旧、case-only rename、日本語ファイル名を確認する。遅延や失敗を注入できる既存テスト境界を利用し、macOS/Windowsで異なるfilesystem動作は実OSで確認する。

原文と未保存編集を失わず、結果の受理・破棄が操作identityに基づき、保存・命名規則がViewとsessionに二重実装されていなければ完了とする。書込みの原子性と電源断時の永続性は同じ保証ではないため、確認できた範囲を明記する。

## RF7 — 開発・検証・リリース基盤を簡素化する

### 作業

**現行GUI検証の重複を確認して統合する。** 日付バッジ、sidebar chrome、汎用GUI validationなどのworkflowについて、共通のcheckout、build、scenario実行、artifact収集を比較する。共通部分が実際に重複している場合だけ、既存runnerや小さな共通部品へ寄せる。scenario固有の再現操作・期待値は残す。[S17]

**権限と判断を統合しない。** trustedなorchestrationとPRコード、reviewとimplementation、GUI観測とmerge判断を分けたままにする。exact-head guard、必要なbase context、認証情報の分離、`unknown` を成功にしない扱いを維持する。AADW v2の判断をworkerやworkflowへコピーしない。[S18]

**CIを整理し、弱めない。** 現行のmacOS/Windowsのworkspace testsとclippy、必要なvendor/GPUIテストを保つ。Python/ShellとRustの変更範囲に対する検証対応を明確にし、path filterの判定失敗を安全側に倒す動作を維持する。通常buildとinstrument buildのどちらも、必要な確認範囲に含める。[S19]

**既存リリースを壊さない。** `windows-release.yml` には、mainのCargo.toml変更、version tag、手動実行によるリリース経路と、Cargo versionとの整合確認がある。新しいrelease機構を作らず、この経路を維持する。リファクタリングPRに意図しないversion更新やtag作成を混ぜない。[S20]

**依存とvendorは別の根拠で整理する。** 未使用依存、不要feature、計測用依存の製品混入、重複versionを確認する。Hane固有vendor patchは、目的、適用先、再現テスト、上流での対応状況、解除条件を記録する。同等修正を確認せずに「vendorが大きい」という理由で消さない。[S11] [S19]

### PRの単位と完了条件

workflowの共通化、CI整理、release整理、依存整理は別PRとする。製品の構造変更と同じPRに入れない。

通常経路、代表的失敗、stale head、権限不一致、artifact不足を確認し、誤った成功・危険な実行が起こらないことを示す。重複する実装・不要な実行が減り、同じ手順を一つの正本から辿れれば完了とする。新しい管理基盤を作っただけでは完了にしない。

## RF8 — 測定で示された無駄を最適化し、完了判定する

### 作業

**局所最適化はプロファイルを根拠に行う。** 例えば `SourceMap::visual_to_source` には候補Vecの構築、sort、可視候補Vecの構築がある。これは調査候補であり、測定前にボトルネックとは断定しない。実際にhot pathなら、Bias・可視候補優先・同順位の扱いを固定したテストの下で、allocationやsortを避けられるか検討する。[S21]

同様に、繰り返されるprojection生成、clone、rowごとの走査、過剰なcache無効化、session保持を計測する。最適化ごとに、変更前後の処理量、時間、allocationやRSSを記録する。計測対象を移しただけで速く見せない。

**同じ条件で全体を比較する。** Issue #23にあるreference macOS環境の初期契約を起点とする。warm startupは150 ms以下を目標、cold startupは400 ms以下、空editor RSSは65 MiB未満、10 MB文書は120 MB未満、100 MB文書は350 MB未満である。これらは今回の実測結果ではない。同条件で10%超の悪化が出た場合は再測定する既存方針を継承し、Windowsへ同じ絶対時間を無条件には適用しない。[S7]

入力とscrollでは、既存のp95/p99契約に加えて、巨大単一blockと大きなwork folderを確認する。平均値だけで長い停止を見落とさない。cold/warm状態、MB/MiB、サンプル条件を明示する。

**移行の残骸を除く。** 一時adapter、旧名のalias、二重経路、temporary feature flagを撤去する。旧経路が必要なら理由を確認し、未完の移行を「互換性のため」として放置しない。現行設計と運用手順を実装に合わせる。

### 最終受入条件

| 項目 | 完了の判断 |
|---|---|
| 棚卸し | 全対象領域を確認し、候補ごとに削除・統合・修正・根拠付き維持の判断がある。対象内の未判断を完了扱いしない。 |
| 所有権 | 入力先、文書意味、位置計算、保存判断、cache無効化の責任者が明確になっている。 |
| 削除 | 確定した不要物と移行用旧経路が取り除かれ、参照・設定・文書も整合している。 |
| 正しさ | source、selection、IME、undo/redo、保存・復旧の契約が維持されている。 |
| 性能 | 同条件の前後比較があり、関連する回帰が未解決のまま残っていない。 |
| 運用 | current headに対するCI・review・必要なGUI証拠があり、権限境界とrelease経路を保っている。 |
| 保守性 | 同じルールを複数箇所に追加しなくても変更できることを、代表的な既存修正の追跡で説明できる。 |

LOC、公開API数、重複分岐数、workflow数、build時間などは前後を報告するが、「何%削減」を先に成功条件にはしない。テストを消した量や単なるファイル移動は改善量として数えない。

## 6. 実施順とPR運用

基本順は `RF0 → RF1 → RF2 → RF3/RF4/RF6 → RF5 → RF8` とする。RF7は、RF0で現行入口を固定した後、製品変更と競合しない範囲で進められる。RF1の旧経路除去とRF7の現行基盤の改善は目的を分ける。

同じ巨大ファイル・同じ状態を変更する作業は重ねない。一方、無関係な変更まで全面的に直列化したり、mainを長期間凍結したりしない。target branchの進展は変更内容への影響で判断し、`behind > 0` だけで毎回同期しない。[S18]

### 各PRの共通ルール

- 一つの責務・一つのroot-cause clusterを扱う。移動、意味変更、最適化、機能追加を混ぜない。
- 着手時に対象head、current main、対象Issue、関連ADR、関連PRを再確認する。この文書のパスや判断が現行実装と食い違う場合は、根拠を示して修正する。
- before/afterで守る契約、削除理由、変更対象外、テスト、必要なGUI・性能確認、revert方法をPRに記載する。
- headが変われば古いCI・review・GUIをcurrent証拠として扱わない。base変更は採用する証拠の主張への影響を確認する。
- データ損失、誤編集、権限問題、通常経路の明確な不具合、受入条件を満たさない変更は通さない。根拠不足のunknownも通さない。
- 無関係な改善まで全件修正することは要求しない。ただし、今回解消すると定めた対象内の負債をfollow-upへ移しただけで全体を完了にしない。

merge前にはexpected head、必要なbase context、required CI、必要なGUI、mergeabilityを確認する。review指摘件数をゼロにすること自体は目的にしない。[S18]

### 検証コマンドと適用範囲

Rust製品変更では、現在のCIの主要条件を保つ。

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

通常feature構成とrelease成果物に影響する変更では、既存のrelease側の条件も確認する。

```sh
cargo test --workspace --locked
cargo build --release --locked -p hane
```

macOSのGPUI patchに影響する場合は、workspace外のvendorテストを省略しない。

```sh
cargo test --manifest-path vendor/gpui/Cargo.toml --features runtime_shaders --lib platform::mac::text_system::tests
cargo test --manifest-path vendor/gpui/Cargo.toml --features runtime_shaders --lib platform::mac::platform::tests::keyboard_selection_change_reactivates_text_context_once_and_ignores_other_responders -- --exact
```

これらは今回実行したコマンドではなく、実施時の検証条件である。Python/Shell/workflowの変更は、現行CIにある関連テストと変更した実行入口を検証する。Rustに影響しない変更に全製品GUI試験を機械的に追加しない。一方、native IME、画面geometry、filesystemの挙動は、対応するunit test成功だけで実OS検証の代わりにしない。[S19] [S20]

### 最初のPR単位

| 順番 | 内容 | 混ぜないもの |
|---|---|---|
| 1 | 旧計画と現行実装の照合、対象台帳、契約テストと性能測定の不足整理 | 製品の意味変更や新機能 |
| 2 | 到達性を確認できた旧AADW実行経路の削除 | 現行GUI基盤の全面再設計 |
| 3 | `view.rs` のサイドバーなど、競合しない一責務の機械的分離 | 条件分岐の修正や保存規則の変更 |
| 4 | 入力先決定の集約と、短い入力欄の所有状態の整理を適切な差分に分けて実施 | 本文Editorやnative IMEの全面置換 |

この後、進行中の描画修正を取り込んだ基準からRF4〜RF5を進める。計画の核心は「削れるものを先に削り、責務を分け、正本と寿命を整理してから、残った無駄を測って除く」ことである。

## 調査資料

以下は調査基準SHAまたは調査時点のGitHub情報である。実施時にはcurrent factsを再取得する。

[S1]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/docs/refactor-execution-plan.md
[S2]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/docs/architecture.md
[S3]: https://api.github.com/repos/hide212131/hane/git/trees/74b79215fbefeff86c57ec3fa9e0a05bb60e30b4?recursive=1
[S4]: https://api.github.com/repos/hide212131/hane/git/trees/12d6ba1710541e35a4566835d6f32f59fdfa8a50?recursive=1
[S5]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/crates/ui/src/actions.rs
[S6]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/.github/scripts/final_judge_probe.py
[S7]: https://github.com/hide212131/hane/issues/23
[S8]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/crates/ui/src/view.rs
[S9]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/docs/adr/0025-shared-parse-for-multiline-inline-presentation.md
[S10]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/docs/adr/0029-source-first-list-editing.md
[S11]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/Cargo.toml
[S12]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/crates/ui/src/line.rs
[S13]: https://github.com/hide212131/hane/pull/292
[S14]: https://github.com/hide212131/hane/pull/295
[S15]: https://api.github.com/repos/hide212131/hane/git/trees/dcbbc8967ca13a37d83934e072a0d3dea6136f5e?recursive=1
[S16]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/crates/session/src/service.rs
[S17]: https://api.github.com/repos/hide212131/hane/git/trees/7645b356ac342012d2210930506859c0fef4c3a8
[S18]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/docs/aadw-command-policy.md
[S19]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/.github/workflows/ci.yml
[S20]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/.github/workflows/windows-release.yml
[S21]: https://github.com/hide212131/hane/blob/ef9356c52bae06dee6cf49fa7b1a59ce004b52a0/crates/presentation/src/lib.rs

[S22]: https://api.github.com/repos/hide212131/hane/git/trees/b2b5b8278300e26e5679907ed09b084eb6ba4064
[S23]: https://api.github.com/repos/hide212131/hane/git/trees/d0cfef526821c1dfe799eb5956e891303b527d12?recursive=1
