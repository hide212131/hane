# エディタ本文のアクセシビリティ合成ノード公開設計

作成日: 2026-11-09  
状態: 採用済み設計（Issue #704、2026-11-09）。製品実装・実機検証は未実施。  
保存先: `docs/implementation/issue-704-a11y-synthetic-nodes-design.md`（本ファイル）

## 1. 目的と設計の前提

カスタム描画されるエディタ本文の構造を、GPUI の合成 AccessKit ノードとして macOS AX / Windows UIA に公開する。

最初の製品スコープは、文書ルートと表示範囲内の起動可能なリンクとする。スクリーンリーダー利用者が表示中のリンクを認識して起動でき、agent-desktop 等がリンクの役割・名前・位置・起動能力を構造として検証できる状態を目指す。

これは本文の完全なアクセシビリティ対応ではない。全文の連続読み上げ、テキスト範囲、選択、キャレット、文字単位移動、編集操作、IME 状態、点字表示との同期は別の契約とする。初期段階では画面外のリンクも列挙しないため、文書全体のリンク一覧として利用できるとは説明しない。

確認済みの実測は、2026-11-09、head `04bbb129` で agent-desktop snapshot に本文・リンクが存在せず、対話要素がクロム側の10件のみという利用者提供の記録である。本設計で再実測した結果ではない。

コード参照には次を用いた。

- メインチェックアウト: `27b5ebc2e04d4c25ec30aca624fe0e9fe4b106ba`
- Issue #700 worktree: `04bbb129`
- 実際の依存である `gpui-pre 0.3.6` の registry ソース
- `accesskit 0.24.1`、vendored `accesskit_windows 0.34.0`

`vendor/gpui` の存在だけで有効な実装と判断せず、Cargo の依存解決先を確認する。実装開始時には current main、関連 Issue / PR、並行開発手順を読み直す。#297 全体の完了を前提条件にはしない。

## 2. 公開する構造とスコープ

### 2.1 ノード種別

| 種別 | 方針 | 理由 |
| --- | --- | --- |
| 文書ルート | Phase 1で公開 | 本文とクロムを区別し、リンクの共通親を提供する |
| リンク | Phase 1の必須対象 | 既存の意味情報・位置計算・起動経路を利用でき、認識と操作の価値がある |
| 見出し | Phase 2で公開 | 文書構造の認識に有用だが、リンクとの読み上げ重複と階層表現を先に確認する |
| 通常段落・本文テキスト | 初期対象外 | 部分的なラベル公開を全文読み上げや編集可能テキストと誤認させないため |
| 画像 | 後続 Issue | alt、装飾画像、リンク内画像、読み込み失敗を含む意味づけが必要 |
| 表・表セル | 後続 Issue | 行列・見出しセル・読み順を一体で設計する必要があり、セル単独公開では不十分 |
| Mermaid・数式等 | 後続 Issue | 代替説明とソースのどちらを読むかを別途定義する |
| タスクチェックボックス | 本設計の新規対象外 | リンクとは異なる状態変更操作であり、既存公開との重複を含めて別途確認する |

文書ルートは `Role::Document` を第一候補とする。名前は文書表示名、説明はモードと公開範囲を表す短い文言とする。絶対パスや本文全体を名前に含めない。

完全なテキスト契約が成立するまで `Role::TextInput`、`TextRun`、テキスト選択属性、値設定アクションを付けない。`Document` を公開しただけで UIA TextPattern 等が提供されるとは扱わない。

リンクは `Role::Link`、表示ラベル、bounds、`Action::Click` を持つ。対象は既存 presentation が起動可能として認識し、既存リンクオープン経路が受け付けるリンクに限定する。初期基準は現在の HTTP(S) 外部リンクであり、相対パス等の対応を本機能で独自に広げない。

ラベルは Markdown の構文記号や destination ではなく、表示文字列から生成する。同一 destination の別出現は別ノードとする。

### 2.2 編集モードと閲覧モード

両モードで同じリンクを公開し、AX/UIA からの意味的起動を提供する。

| 操作 | 編集モード | 閲覧モード |
| --- | --- | --- |
| 通常のマウスクリック | 既存の選択・キャレット操作 | 既存のリンク起動条件に従う |
| マウスの修飾キー付きクリック | 既存条件に従う | 既存条件に従う |
| AXPress / UIA Invoke | リンクを起動 | リンクを起動 |

AX/UIA 起動に Command / Ctrl の押下を要求しない。意味的なリンク起動要求を、編集用の通常クリックへ変換しない。

モード切替だけでは同一リンクの ID を変更しない。ルートのモード説明と、表示内容・bounds は更新する。

## 3. GPUI への組み込み

`gpui-pre 0.3.6` では、実要素に ID と role がある場合にアクセシビリティノードが作られ、prepaint 後に `a11y_synthetic_children` が呼ばれる。

`A11ySubtreeBuilder` が提供する主な API は次のとおり。

- `synthetic_node_id(key)`
- `push_child(id, node)`
- `parent_node()`

`push_child` は合成 leaf を追加する API である。初期段階では文書ルート直下へリンク leaf を並べ、合成ノードだけで任意の階層を組めるとは仮定しない。

推奨構成は、既存本文レイアウトを変えずに、本文の実要素を文書ルートとして扱う薄いカスタム要素で包む構成とする。

1. 既存の本文要素を通常どおり layout / prepaint する。
2. a11y 活性時だけ、同じフレームの可視リンク情報を収集する。
3. `a11y_synthetic_children` でノードを追加し、生成した ID を prepaint state に保持する。
4. paint 時に、その ID へ `Window::on_a11y_action` を登録する。

builder 自体にアクション登録 API はない。宣言した Click と、実際の ID 別リスナー登録を分けて実装する必要がある。

ノード情報とハンドラー情報を別々に再計算せず、同じフレームの公開レコードから生成する。製品の文書モデルに a11y 用状態を保存しない。

## 4. ノード ID とフレーム間同一性

### 4.1 親要素

GPUI の `GlobalElementId` は祖先の ID を含む。子のローカル ID が同じでも、祖先構造が変わると AccessKit ID が変わる。

本文ルートの ID 経路は、スクロール・ズーム・モード・テーマで変化しない構造にする。表示順、行番号、画面座標、タブの並び順を親 ID に使わない。

文書の識別には既存 `DocumentKey` の session / generation を利用する。ウインドウ内で別文書・別世代が同じノードとして扱われないことを保証する。

### 4.2 Phase 1 のリンク ID

初期段階では、安全側の明確な契約を採用する。

```text
文書ルートの ID
  + ノード種別
  + 文書の a11y 内容世代
  + link_text_range
```

このキーを `builder.synthetic_node_id(key)` に渡す。最終 NodeId を独自の数値規則で直接構成しない。

- `link_text_range` はリンク全体の識別に使う。
- soft wrap や複数の物理行に分かれても、同じリンクは1ノードとする。
- 同一 URL の別出現は異なる範囲なので区別できる。
- 本文変更、Undo/Redo、再読込、reference 定義変更等でリンクの意味が変わり得る場合、内容世代を更新する。
- 内容世代はウインドウの存続中に再利用しない。
- 内容が変わらないスクロール、ズーム、テーマ変更、タブ往復では ID を維持する。

初期段階では、本文変更時に全リンクの ID が変わり得る。この制約は受入条件と利用者向け説明に明記する。編集中の読み上げ位置が失われる問題を隠して、編集アクセシビリティの完成とは扱わない。

非活性中に変更があった場合は、再活性化時に新しい公開世代を割り当てる。非活性時にも a11y 専用の編集追跡を続ける必要はない。

画面外へ出たノードはツリーから除去する。内容世代が同じなら再表示時にも同じキーを生成できるが、OS・スクリーンリーダーが探索位置まで復元する保証はない。

### 4.3 Phase 2 の改善

編集をまたぐ同一性は、既存の編集 delta とソース対応を使って拡張する。

- 変更されていないリンクは位置を追随させ、トークンを保持する。
- リンク本文・destination・reference 解決結果が変わった場合は新しいトークンにする。
- 同一性を証明できない再 parse、分割、結合、文書置換では作り直す。
- `BlockId` だけでリンク同一性を判断しない。ブロック内容が変わっても ID が保持される場合がある。
- トークンを削除後の別リンクへ再割当てしない。

表示番号、URL 単独、ラベル単独による照合は採用しない。

## 5. bounds と更新トリガー

### 5.1 座標源

リンクのソース範囲、source map、行 layout、shaper、表セル fragment、および既存のリンク矩形算出を再利用する。AX 用に Markdown を再 parse したり、文字数と平均幅から位置を推測したりしない。

参照箇所:

- `crates/ui/src/view/render.rs`
- `crates/ui/src/view/link_hover.rs`
- `crates/ui/src/view.rs` の `row_hit_at_window_point`、`content_y_for_row`
- `crates/ui/src/line.rs` の `link_visuals`
- `crates/presentation/src/display.rs` の `LinkTarget`
- `crates/presentation/src/layout.rs`

hover 状態は「現在の1リンク」であり、全可視リンクの公開元にはしない。範囲と位置計算を共有し、ポインターがない状態でも列挙できるようにする。

合成 leaf の bounds は明示設定する。実要素と同じ自動 bounds 付与を期待しない。

座標変換は次の順序とする。

```text
ソース範囲
→ 表示 fragment の矩形
→ 行・セル内の位置
→ 本文の window-local logical 座標
→ 本文 viewport と clip の交差
→ GPUI と同じ scale_factor による AccessKit 座標
→ OS adapter による画面座標
```

Hane 側でスクリーン原点を足さない。GPUI の通常ノードは logical bounds に `window.scale_factor()` を掛けており、合成ノードも同じ単位にそろえる。ズームによるレイアウト拡大と display scale を混同しない。

### 5.2 複数 fragment

同じリンクの可視 fragment を集約し、非空の可視矩形の外接矩形をノードの bounds とする。

外接矩形の中央がリンク文字上にあるとは限らない。リンク起動は ID に対応する意味的アクションで行う。座標によるテストでは外接矩形の中央を無条件にクリックせず、実際の文字 fragment 内を使う。

これは単一矩形で複数行リンクを表す際の制約である。領域内の全点がリンクの物理クリック対象である、という保証は提供しない。

### 5.3 更新

| トリガー | 更新内容 |
| --- | --- |
| 編集・Undo/Redo・再読込 | 意味情報、ラベル、ID 世代、bounds を更新 |
| reference 定義の編集 | 定義と離れたリンクも再検証 |
| スクロール・慣性スクロール | 可視集合と bounds を更新。内容世代は維持 |
| ズーム・resize・サイドバー幅変更 | wrap と bounds を再計算 |
| display scale・モニター変更 | scale と bounds を更新 |
| タブ切替・文書置換 | 親文書を切替え、旧文書の起動を拒否 |
| 編集／閲覧モード切替 | 説明と表示情報を更新 |
| テーマ・フォント変更 | メトリクス変更時は再 layout。色変更だけなら ID を維持 |
| a11y 活性化 | 初回の完全な可視ツリーを生成 |
| 文書を覆う modal 等 | 既存 UI の modal 制約に従い、背面本文の起動を拒否 |

古いフレームの矩形と新しい文書の destination を混在させない。

## 6. アクションとフォーカス

### 6.1 リンク起動

```text
AXPress / UIA Invoke
→ AccessKit Action::Click
→ NodeId に登録した GPUI リスナー
→ 現在の文書・公開世代・対象リンクを検証
→ 既存のリンク起動処理
→ 既存の cx.open_url 等
```

マウス経路と a11y 経路は、対象解決後の「リンクを開く」処理を共有する。scheme 判定や routing を a11y 側に複製しない。共通化が必要なら、対象解決と副作用を分ける最小の整理として扱う。

`on_row_mouse_down`、`apply_row_click` をそのまま呼び出さない。これらには focus、modifier、ドラッグ開始、selection 更新が含まれるためである。

GPUI の既定 Click fallback も使わない。通常ノード用の bounds 中央へ修飾キーなしのマウス入力を生成する動作では、編集モードで誤編集するおそれがある。合成ノードは通常要素と同じ内部 bounds 登録も自動では得られない。

リスナーは毎フレーム登録する。GPUI はフレーム開始時にリスナーをクリアするため、初回だけの登録では成立しない。

### 6.2 stale action の扱い

起動直前に少なくとも次を確認する。

- 対象文書が現在の active document と一致する。
- 文書 generation と公開内容世代が一致する。
- 対象リンクが現在も公開対象である。
- destination が現在の presentation と一致する。
- 本文を操作できない modal 状態等でない。

一致しなければ何も開かず、文書・選択も変更しない。古い要求を同じ位置にある新しいリンクへ読み替えない。

UIA Invoke は非同期の要求であり、戻り値成功だけではブラウザでの表示完了を証明できない。UI スレッドでブラウザ完了を待たない。[Microsoft IInvokeProvider::Invoke](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationcore/nf-uiautomationcore-iinvokeprovider-invoke)

### 6.3 フォーカス

Phase 1ではリンクへ独立した GPUI keyboard focus を新設せず、Focus / Blur アクションや独自の Tab 順序を宣言しない。

スクリーンリーダーの探索位置と実キーボードフォーカスを区別する。リンク起動だけでキャレットを移動せず、既存の editor focus を不用意に奪わない。外部ブラウザへの OS の前面移動は既存経路に従う。

両 OS の支援技術が、keyboard-focusable でないリンクを探索・起動できるかを Phase 0で確認する。成立しなければ受入条件を緩めず、必要な Focus 契約を設計し直す。

起動によって次が変わらないことを必須とする。

- 本文 bytes、revision、dirty state
- selection、caret
- Undo/Redo
- IME composition

## 7. 性能ガードと画面外リンク

「a11y 非活性時のコストゼロ」は、本機能による追加の列挙・コピー・計算・割当て・登録がないことと定義する。活性確認の定数分岐まで含めた文字どおりのゼロ命令とは主張しない。

`window.is_a11y_active()` を、a11y 専用処理の入口に置く。

非活性時には次を行わない。

- リンク一覧の生成
- ラベル生成・文字列コピー
- bounds の集約・変換
- NodeId の hash
- アクション closure の生成・登録
- a11y 専用の編集追跡や background task

既存の hover、クリック、描画に必要な処理は従来どおり動作する。そのコストを本機能の追加コストと混同しない。

活性時も既存の可視 layout から収集し、全文走査・全リンクの layout を行わない。画面外に描画される overscan 部分は viewport と交差しなければ公開しない。画面外リンクを偽のゼロ矩形で残さない。

長大なリンクでは、初期段階のラベルは可視表示文字列から構成し、部分表示であることを説明属性で伝える。全文ラベル取得のために画面外を新たに layout しない。

性能検証は同じ SHA・profile・fixture で活性／非活性を比較する。処理回数・割当て・frame time を測り、非活性時の専用処理回数ゼロと、活性時の処理量が可視内容に依存することを確認する。数値予算は変更前測定をもとに実装 Issue で固定する。

画面外探索、ScrollIntoView、文書全体のリンク一覧は後続 Issue とする。

## 8. macOS AX / Windows UIA

| 観点 | macOS | Windows |
| --- | --- | --- |
| 検証対象 | AX の link 相当要素、名前、位置、起動 action | Hyperlink、Name、BoundingRectangle、InvokePattern |
| 支援技術 | VoiceOver | Narrator。必要に応じて NVDA を追加 |
| 座標リスク | Retina scale、画面原点、複数モニター | DPI、client/screen 変換、複数モニター |
| 主な寿命リスク | 古い AX 参照、window・文書切替 | COM provider の遅延参照、破棄済み HWND |
| 受入証拠 | AX snapshot と VoiceOver 操作 | UIA snapshot、pattern 確認と実 Invoke |

role 名や action 名が OS 間で一致することを要求しない。同じ意味の認識・起動能力を要求する。AccessKit 内部ツリーの一致だけで OS 公開の成功とは扱わない。

vendored `accesskit_windows` では `Role::Link` は Hyperlink control type に対応し、Invoke は Click に接続される。ただし pattern の公開条件と実機での探索可能性は別途確認する。

Issue #570 のパッチは、`ClientToScreen` が無効 HWND で失敗した場合に、panic ではなく `UIA_E_ELEMENTNOTAVAILABLE` を返すための変更である。合成ノード対応や通常 HWND の bounds 正当性を保証するパッチではない。

本機能ではこのパッチを変更しない。通常 HWND の座標確認に加え、ウインドウ終了後の保持済み参照で crash しないことと、既存の invalid HWND 回帰検証を維持する。通常のノード削除と HWND 失効を同じ失敗原因として扱わない。

## 9. #661 の画面操作 API との関係

両者は補完関係にある。

| 項目 | 本設計 | #661 |
| --- | --- | --- |
| 公開先 | OS AX / UIA | アプリ内の画面操作 API |
| 主目的 | 支援技術と OS 構造検査 | 通常の入力経路を使う再現可能な GUI 操作 |
| リンク操作 | 意味的な起動 | 通常のクリック・入力の配送 |
| 検証できること | role、名前、bounds、OS action | modifier、hit-test、drag、focus、入力配送 |
| ID の契約 | AccessKit のツリー内同一性 | API 固有の操作対象同一性 |

#661 の成功を AX/UIA 公開の evidence にしない。逆に AXPress / Invoke の成功で通常のマウス入力・選択・modifier 判定を検証済みとしない。

両者で既存の presentation・geometry・リンク起動処理を共有できるが、識別子の永続性や寿命を同一の契約にしない。

## 10. テストと実機 evidence

### 10.1 単体・GPUI 統合テスト

**ノード生成と ID**

- 文書ルートと可視リンクの role、ラベル、bounds、Click 宣言。
- 同一 URL の別出現、隣接リンク、reference link の区別。
- soft wrap・複数物理行での1リンク1ノード。
- スクロール・ズーム・テーマ・タブ往復による ID 維持。
- 文書変更・再読込・Undo/Redo・reference 定義変更による旧 ID 失効。
- 異なる文書、同じファイルの再オープン、複数ウインドウの衝突防止。
- 同一フレームでの重複 ID 検出。

**geometry**

- source map と glyph 計測を用いた範囲。
- 最終 glyph の右半分、隣接境界、絵文字・結合文字。
- soft wrap、表セル内 wrap、引用・リストの字下げ。
- 部分 clip、スクロール、検索バー、サイドバー、ズーム、display scale。
- 複数 fragment の外接矩形と、実際のクリック領域の区別。

**アクション**

- 宣言した NodeId へ Click が届き、正しい destination を1回だけ渡す。
- 編集・閲覧の両モードで修飾キーを要求しない。
- stale request、タブ切替、文書置換、modal 中の要求を拒否する。
- unsupported scheme 等を既存ルールどおり扱う。
- 本文・revision・selection・caret・Undo/Redo・IME の不変。
- 既存の通常クリック、modifier click、drag 選択の回帰。

**性能**

- 非活性時に専用 collector・label builder・handler registration が呼ばれない。
- 活性化後の初回フレームに必要なノードが現れる。
- 同じ可視領域で文書全体だけを増やしても全文処理を開始しない。

単体・GPUI テストは OS の role mapping、支援技術の読み上げ、DPI 変換、native action の成立を証明しない。

### 10.2 実機シナリオ

`docs/agent-desktop-gui-testing.md` に従い、初回は次を読む。

```sh
agent-desktop skills get desktop
agent-desktop skills get platform
```

通常の受入シナリオは対象 head の Debug / development build を使う。Release の結果へ置き換えない。性能確認は使用 profile を別途明示する。

専用 state と fixture を使い、OS、agent-desktop version、対象 SHA、必要な base context、build profile、実 PID、実行パス、起動引数、fixture を記録する。

fixture には少なくとも次を含める。

- 同じ名前・同じ destination の別リンク
- inline / reference / bare URL のうち現在対応している形式
- wrap するリンク、日本語・絵文字
- 見出し内リンク、表セル内リンク
- viewport 境界のリンク、画面外リンク
- 非対応 destination、通常テキスト、画像

各 OS で次を実施する。

1. screenshot と bounds 付き fresh snapshot を保存する。
2. 文書ルートとリンクの意味情報、重複、表示範囲を確認する。
3. スクロール・ズーム・タブ往復後に再取得し、位置と文書帰属を確認する。
4. AXPress / UIA Invoke でリンクを起動する。
5. 編集・閲覧の両モードで destination と編集 state の不変を確認する。
6. リンク削除、destination 変更、タブ切替後に旧参照を使い、誤起動がないことを確認する。
7. VoiceOver / Narrator で探索・読み上げ・起動を確認する。
8. 通常のマウス・キーボード・選択操作を再確認する。

agent-desktop の `click` が native action と座標入力のどちらを使ったかを記録する。座標 fallback の成功だけでは AXPress / Invoke の evidence にしない。ツールで経路を確認できなければ OS inspector 等で補う。

外部リンクの確認には隔離したテスト用 URL を使い、handoff とブラウザ側の観測を区別する。単なる CLI の成功応答を期待結果の証明にしない。

Windows は接続済み Windows チャットから直接操作する。Mac 経由の VM 操作は使わない。画面取得・参照・入力が失敗した場合は `docs/gui-test-recovery.md` に従って復旧し、元シナリオを再実行する。未実施・unknown を pass にしない。

## 11. 段階計画と受入条件

| Phase | 範囲 | 受入条件案 |
| --- | --- | --- |
| 0: 契約確認 | 最小のルート・リンクによる両 OS の技術検証 | 合成 ID への action 配送、座標単位、非活性ゲート、keyboard focus を新設しないリンクの探索・起動が成立 |
| 1: 可視リンク | 両モードの文書ルート・可視リンク、安全な起動、内容世代による失効 | 上記の単体・統合テスト、両 OS snapshot と native action、VoiceOver / Narrator、編集 state 不変、非活性時専用処理ゼロが成立 |
| 2a: 編集時の同一性 | 変更されないリンクの ID 追随 | 前方挿入等で未変更リンクを維持し、変更・削除・曖昧な照合では旧 action を拒否。編集中の読み上げ挙動を実機確認 |
| 2b: 可視見出し | Heading と level、読み順 | 表示範囲の見出しを認識でき、見出し内リンクで同じ文言の不必要な重複や誤起動がない |
| 後続 | 画面外探索、画像、表構造、完全なテキスト契約 | それぞれ別 Issue で目的・性能・OS ごとの受入条件を定義 |

Phase 0の prototype は完成機能として出荷した扱いにしない。Phase 1を受け入れても「本文をすべて読み上げ・編集できる」とは説明しない。

Phase 2a と 2b は別 PR とし、同一性改善と構造追加を混ぜない。いずれの段階も、必要な OS evidence が不足したまま受入・merge しない。

## 12. 未決事項と主なリスク

| 項目 | 現時点の方針・解決条件 |
| --- | --- |
| 実キーボードフォーカスを持たない Link の利用可能性 | Phase 0で両 OS の支援技術から確認。成立しなければ Focus 契約を追加設計 |
| 本文ルートと既存 focus holder の対応 | InputCapture を含めて確認し、同じ focus を複数ノードが主張しない |
| 編集ごとのリンク ID 更新による探索位置喪失 | Phase 1の明示的制約。Phase 2aで未変更リンクの追随を導入 |
| 遅延 action による別文書・別 destination の誤起動 | 文書・世代・現在のリンクを起動直前に検証し、不一致では無操作 |
| 複数行リンクの bounds 内に非リンク領域が含まれる | 意味的起動を使用。座標テストでは実 fragment を使用 |
| source map・layout と公開ツリーの世代ずれ | 同一フレームの公開レコードからノードと action を生成 |
| 画面外リンクを探索できない | 初期制約として明示。全文 layout による解決は採らない |
| 見出しとリンクの階層・読み上げ重複 | builder の leaf 制約を踏まえ、Phase 2bで実機検証して決定 |
| 非活性時に closure やラベルだけ先に生成される | 専用処理全体を活性ゲート内に置き、呼出し・割当てを検証 |
| OS adapter による role filtering や pattern 差 | 内部ツリーと native 公開を別々に検証 |
| window 終了時の provider 寿命 | #570 パッチを維持し、保持済み参照と通常 HWND の両方を検証 |
| 完全な編集アクセシビリティとの混同 | TextInput・範囲・選択を中途半端に宣言せず、別 Issue の契約にする |

最大の安全上のリスクは、古いリンク ID・古い geometry と現在の文書が混ざり、別リンクを起動したり選択を変更したりすることである。ID の失効、同一フレームの情報共有、起動直前の検証、マウス入力へ変換しない action 接続で防ぐ。

最大の利用上の制約は、可視部分だけの公開では文書全体の閲覧を完結できないことである。初期段階の価値と限界を区別して説明する。

---
