# 司令塔の確定事項（2026-11-13、設計ドラフトへの追記）

- **Jev 判定**: 全体判断 noul 0.77（妥当）、右寄せの行構造の組み替えは採用（choice restructure、信頼度 0.52 / 確率 0.76）、`DateBadgePosition` の削除は妥当（noul 0.83）。
- **a11y の行名付与は分離**: 設計ドラフト §5（ファイル行への `Role::Group` と `aria_label` 付与）と関連テスト `file_row_accessible_label_matches_untruncated_display_parts` は本 Issue の実装範囲から外し、フォローアップ Issue #720 とした（Jev が拮抗: follow_up 確率 0.51 / include 0.49。現状でも行名は存在しないため、分離による悪化はない）。実装者はこの部分を実装しないこと。
- **基準コードの確認**: ドラフトはローカル HEAD `27b5ebc2` 基準だが、司令塔が origin/main（`b06d8648`）と照合し、`sidebar.rs` / `sidebar_pins.rs` / `date_badge.rs` は現行 main と同一内容であることを確認済み。設計のコード事実は現行 main でも成立する。
- 上記以外はドラフトの設計どおりとする。

---

# Issue #719 実装設計書

## 概要

日付を、背景のない小さな補助テキストとして表示する。**本日はファイル名と同じ前景色、それ以外は同色の不透明度70%**とし、ファイル行の右側に揃える。

右端のピン操作スロットは引き続き常時確保し、日付の右端をその左境界に揃える。ホバーによって日付の位置は変わらない。名前が長い場合は名前を省略し、日付を優先して表示する。

本書はローカルHEAD `27b5ebc2e04d4c25ec30aca624fe0e9fe4b106ba` のコードを基にした設計であり、コード・ファイルの変更、GUI検証は行っていない。

## 現状の経路整理

| 対象 | 現状 |
|---|---|
| 日付の抽出 | `crates/session/src/date_badge.rs` の `split_file_name_for_badge` |
| 相対日付の文面 | 同ファイルの `format_relative_date_label` |
| 近さの分類 | 同ファイルの `date_badge_range`。週は月曜〜日曜で、月・年より先に判定する |
| ファイル名の描画 | [sidebar.rs](crates/ui/src/view/sidebar.rs#L2292) の `file_name_label` |
| チップの描画 | 同ファイルの `date_badge_chip`。青系背景、白文字、10px、左右4px、角丸 |
| 呼び出し元 | 作業フォルダのファイル行の1箇所。`DateBadgePosition::Right` 固定 |
| ピン操作 | [sidebar_pins.rs](crates/ui/src/view/sidebar_pins.rs#L590) の `pin_toggle_slot` |
| テーマ | [theme.rs](crates/ui/src/theme.rs#L12) のライト／ダーク定義 |

描画は、おおむね次の構造になっている。

```text
ファイル行
├─ アイコン＋ラベル領域：flex_1 / min_w(0) / overflow_hidden
│  ├─ アイコン
│  └─ file_name_label：flex row / min_w(0)
│     ├─ 残りの名前：flex_1 / min_w(0) / truncate
│     └─ 日付チップ：flex_none
└─ ピン操作スロット：24px固定
```

ラベル自体に `flex_1` がないため、短い名前では日付が名前の直後に付く。また、日付だけのファイル名では残りの名前要素を生成しないため、単純にラベルを伸ばしても左側の空間を埋める要素がない。

ファイル行は高さ24px。通常の名前はサイドバーから `BODY_FONT_SIZE = 14px` と `sidebar_foreground` を継承する。リネーム中は `file_name_label` 全体が `inline_rename_label` に置き換わる。

なお、現行の相対日付表示は提示例から更新されており、別年では短縮年の `'25/10/3` などを使う。**本Issueでは現行関数の出力を維持し、提示例に戻す変更も行わない。**

## 設計：変更ファイルと変更内容

| ファイル | 変更内容 |
|---|---|
| `crates/ui/src/view/sidebar.rs` | チップを日付テキストへ置換。配色・レイアウト・a11y名を変更し、関連テストを更新 |
| `crates/ui/src/view.rs` | 不要になる `DateBadgeRange`、`date_badge_range` のUI側importを整理 |
| `crates/ui/src/theme.rs` | 変更しない。既存の前景色・背景色をテストで参照する |
| `crates/ui/src/view/sidebar_pins.rs` | ピン操作の表示・領域・イベント処理は変更しない |
| `crates/session/src/date_badge.rs` | 抽出・文面・近さの分類は変更しない |
| `scripts/hosted_date_badge_gui.py` | 既存検証を再利用。右端揃えの保証は追加のGPUIテストで行う |

### 描画関数

`date_badge_chip` を `date_badge_text` に改名し、次のスタイルとする。

```rust
div()
    .flex_none()
    .whitespace_nowrap()
    .text_color(rgb(theme.sidebar_foreground).opacity(alpha))
    .text_size(px(10.0))
    .child(format_relative_date_label(date, today))
```

背景、角丸、左右4pxのチップ用余白を削除する。`alpha` は `date == today` なら `1.0`、それ以外なら `0.7` とする。

### レイアウト

基本となる変更は、ラベル行への `flex_1().min_w(px(0.0))` の追加、名前側への `truncate`、日付側への `flex_none` である。

ただし現行のファイル行では階層インデントが外側の左パディングに含まれている。深い階層でも日付を守るため、最終構造は次のようにする。

```text
ファイル行：高さ24px、左右の基本余白、アクティブ背景
├─ 内容領域：flex_1 / min_w(0)
│  ├─ 名前領域：flex_1 / min_w(0) / overflow_hidden
│  │  └─ インデント付き内容：flex row / items_center
│  │     ├─ ファイルアイコン
│  │     └─ 名前：flex_1 / min_w(0) / truncate
│  └─ 日付：flex_none / nowrap
└─ ピン操作スロット：24px固定
```

日付をインデント付き内容の外へ出す。深い階層では名前領域が先に縮み、インデントやアイコンもその領域内でクリップされる。これは既存のフォルダ行が、深いインデントで操作ボタンを押し出さないために使っている構造に合わせる。

`file_name_label` はこの内容領域を組み立てる責務にし、必要なインデント量を受け取る。呼び出し元にある通常表示用のファイルアイコンも、この名前領域へ移す。リネーム表示も同じ名前領域の配置を使う。

日付だけの名前でも、左側の名前領域は空の可変領域として残す。日付なしの場合は名前領域だけを表示する。

## 判断と理由

### 1. 近さの5段階：本日とそれ以外の2段階へ変更する

**案(c)を採用する。**

| 案 | 評価 |
|---|---|
| 一律の薄い文字色 | 最も静かな表示だが、本日の視覚的な手掛かりも失う |
| 5段階の不透明度 | #211の順序を残せるが、読める範囲に限定すると段階差が小さく、10px文字では識別を保証しにくい |
| 本日は通常色、それ以外は薄くする | 今回の要望に沿って装飾を減らし、本日の手掛かりだけ残せる |

#211の「5段階の近さを見分ける」意図は、そのままでは維持しない。今回の「浮かない・薄い表示」を優先し、本日以外の近さは既存の日付文面で判断できるようにする。

これは表示仕様の変更であり、近さの分類関数自体の仕様変更ではない。session側の `DateBadgeRange` とそのテストの削除は、本Issueに含めない。

### 2. 薄さ：既存前景色のalphaを変更する

**テーマの弱い前景色フィールドは追加しない。**

依存している `gpui-pre 0.3.6` のソースで、次を確認した。

- `rgb(u32)` はalphaが `1.0` の `Rgba` を生成する。
- `Rgba::opacity(factor)` はRGBを維持し、alphaへ係数を乗算する。
- `truncate()` は `overflow_hidden`、折り返し禁止、省略記号を設定する。

したがって、文字色だけに次を適用する。

```rust
rgb(theme.sidebar_foreground).opacity(0.7)
```

要素全体の `.opacity(0.7)` は使わない。日付文字だけのスタイルであることを明確にし、将来子要素が増えても一緒に薄くならないようにする。

読める下限は、**通常背景とアクティブ背景の双方に対してコントラスト比4.5:1以上**を設計上の受入基準とする。現行パレットで、不透明度70%の文字を背景に合成して計算した概算値は次のとおり。

| テーマ・行状態 | 背景 | 合成後の文字色 | コントラスト比 |
|---|---:|---:|---:|
| ライト・通常 | `#f0ede7` | `#636260` | 約5.21:1 |
| ライト・アクティブ | `#e0dcd3` | `#5e5d5a` | 約4.81:1 |
| ダーク・通常 | `#18191b` | `#aaa8a4` | 約7.41:1 |
| ダーク・アクティブ | `#2c2d30` | `#b0aeaa` | 約6.22:1 |

65%ではライト・アクティブ行が約4.20:1となるため採用しない。テストでは合成後の浮動小数点RGBから計算し、上表の丸めた色値を固定仕様にはしない。

将来テーマを変更した際も、同じテストで不足を検出する。

### 3. 文字サイズ：10pxを維持する

日付は補助情報として10pxのままとする。

14pxへ揃えると周囲との統一感は増すが、別年・曜日付きの日付が横幅を取り、最小幅160pxのサイドバーで名前を表示できる領域が減る。背景と余白をなくすだけでもチップの強さを減らせるため、本Issueではサイズまで拡大しない。

行高24px、名前14pxは維持し、名前領域・日付領域は `items_center` で垂直中央に揃える。日付は折り返さず、行高を押し広げない。

ただし、コントラスト比の合格だけで10px文字の読みやすさが保証されるわけではない。実機の通常表示倍率で確認する。

### 4. 右寄せ、ピン、リネーム、truncate

右寄せの基準は、**行内のピン操作スロットを除いた内容領域の右端**とする。パネル外縁には既存の余白・スクロールバー領域もあるため、日付をパネル外縁へ直接合わせる設計ではない。

- ピンの24px領域は常時確保する。
- 現行どおり、通常時はスロットが不可視で、行ホバー時に可視となる。幅は変わらない。
- 日付はピンの有無・ホバー状態によらず同じ右端へ揃う。
- 名前領域を `flex_1 + min_w(0)`、名前文字を `truncate` とし、日付には省略を適用しない。
- 日付だけの名前でも空の可変領域を残し、右寄せを成立させる。
- 日付なしの名前も同じ可変領域で省略する。
- リネーム中は現行どおり日付表示を外し、編集入力と固定拡張子を表示する。確定・取消後に日付表示を再生成する。
- アクティブ背景は外側のファイル行が描画する。日付要素に背景を置かないため、選択背景が連続して見える。
- 日付自体にクリックハンドラを追加せず、行のファイルを開く・リネームする操作へ引き続き伝播させる。

### 5. アクセシビリティ：現状の不足を明示し、名前を明示する

**現行コードでは、ファイル行のa11y名前に日付が含まれているとは確認できない。**

ファイル行には `.id(...)` があるが、roleと `aria_label` がない。日付・名前は通常の文字列子要素である。依存GPUIでは、Divのa11yノード生成にはroleが必要で、通常の `SharedString` は独立したa11yノードを生成しない。見た目の文字列が自動連結される前提は置けない。

本Issueでは通常表示中のファイル行に、既存IDを使って `Role::Group` と明示的な `aria_label` を付ける。ピンボタンは独立した子コントロールとして維持する。

名前は描画と同じ抽出結果・同じ相対日付文字列から作る。

| 表示 | a11y名前 |
|---|---|
| 名前＋日付 | `会議メモ、本日` |
| 日付のみ | `本日` |
| 日付なし | 省略前の表示ファイル名 |

視覚上の省略記号はa11y名前に使わない。日付を別ノードにも追加して重複読み上げさせない。

リネーム中は通常表示用のラベルを外し、古い相対日付を読み上げ名として残さない。編集入力のa11y全般の改善は別範囲とする。実際の読み上げ結果は実機確認を必要とする。

### 6. 既存テスト：背景段階の契約を置き換える

`sidebar.rs` の既存テスト  
`date_badge_palette_has_visible_steps_and_readable_light_text` は削除し、前景色・透明度と背景合成後のコントラストを検証するテストへ置き換える。

旧背景定数と `date_badge_background` も削除する。「青の5段階」や「白文字」の期待値だけを変更して、旧仕様のテストを残すことはしない。

抽出、相対日付文面、日付更新のテストは維持する。

### 7. `DateBadgePosition`：削除する

`DateBadgePosition`、`badge_renders_before_remainder`、位置引数、左右順序のテストを削除する。

現在の呼び出しはRight固定の1箇所だけであり、今回の仕様は右端揃えである。将来用Leftを残すと、右端揃えの契約と未使用の分岐を同時に保守することになる。位置設定を実装するIssueが成立した時点で、必要なレイアウトとして設計し直す。

## テスト計画

### 純粋関数のテスト

`crates/ui/src/view/sidebar.rs` に次を追加する。

| テスト名案 | 検証内容 |
|---|---|
| `date_badge_text_uses_full_opacity_only_for_today` | 本日1.0、それ以外0.7。未来日も本日以外として扱う |
| `date_badge_text_contrast_is_readable_on_both_sidebar_backgrounds` | 両テーマの通常・アクティブ背景で4.5:1以上 |
| `file_row_accessible_label_matches_untruncated_display_parts` | 名前＋日付、日付のみ、日付なし。日付は一度だけ含む |

既存の以下は維持する。

- `display_file_name_omits_only_a_trailing_md_extension`
- `display_file_name_keeps_date_badge_split_and_title`

`crates/session/src/date_badge.rs` の抽出・文面・分類テストは変更せず再実行する。特に `today_is_labeled_as_such`、`future_dates_never_show_a_weekday`、月・年境界の文面テストを含める。

### GPUIの実レイアウト・イベントテスト

`crates/ui/src/view/sidebar.rs` に、行ごとに識別できるテスト用selectorを追加して検証する。固定した「今日」を使う描画ハーネスにより、実行日への依存を避ける。

| テスト名案 | 検証内容 |
|---|---|
| `date_badge_text_aligns_to_the_content_right_edge` | 短名・長名で日付右端が一致し、ピンスロット左端に揃う |
| `date_only_file_name_keeps_the_date_right_aligned` | 名前部分が空でも右寄せされる |
| `long_and_deep_file_names_do_not_clip_date_badge_text` | 最小幅、長い日本語名、深い階層、長い日付文面で日付全体が内容領域内に収まる |
| `pin_hover_does_not_move_date_badge_text` | ホバー前後で日付boundsが不変 |
| `inline_rename_hides_and_restores_date_badge_text` | リネーム開始で非表示、確定・取消で復帰 |
| `clicking_date_badge_text_opens_its_file_row` | 日付部分へのクリックが行の操作として届く |

行高24pxと日付の縦方向の収まりも検証する。実際の省略記号、文字の読みやすさ、背景の見え方はGUI確認で補う。

既存回帰テストとして、以下も実行する。

- `sidebar.rs`：`deep_row_buttons_stay_reachable_at_minimum_sidebar_width`
- `sidebar_pins.rs`：`clicking_the_toggle_pins_once_without_opening_the_row`
- `view.rs`：`sidebar_file_f2_escape_and_enter_rename_without_touching_the_extension`
- `view.rs`：`apply_sidebar_date_badge_today_notifies_only_when_the_date_actually_changes`

### GUI確認

対象HEADのDebug／development buildを使い、ライト／ダークそれぞれで確認する。

- 本日とそれ以外、通常行とアクティブ行。
- 短名、長い日本語名、日付のみ、日付なし。
- 最小幅160pxと通常幅、深い階層。
- ピンのホバー表示・切替時に日付が移動しないこと。
- リネーム開始・取消・確定。
- a11yツリー上の名前と、利用可能な読み上げ環境での日付の重複・欠落。

既存の `scripts/hosted_date_badge_gui.py` は日付文面・抽出・名前の右側にあることの確認に再利用できる。ただし、現行の `badge_is_strictly_right` は**行の右端への整列を保証しない**ため、その合格だけで本Issueの右寄せを合格扱いにしない。

基本の実行対象は次のとおり。

```bash
cargo test -p hane-session date_badge
cargo test -p hane-ui date_badge
cargo test -p hane-ui file_row_accessible_label
cargo fmt --all -- --check
python3 -m unittest discover -s scripts/tests -p test_hosted_date_badge_gui.py
```

追加・既存のGUIイベントテストは、対応する実行環境で個別に実行する。

## リスク・未確認点

- **10px文字の実機での読みやすさ**は未確認。計算上のコントラスト合格と、見た目の読みやすさを別に確認する。
- **右端の定義**はピン操作スロットの左境界とする。ピン領域を含む物理的な行末への配置は採用しない。
- **深い階層と極端な狭幅**では名前やアイコンがほぼ見えなくなる可能性がある。日付を優先するが、日付自体より狭い領域での完全表示は不可能である。最小幅160pxでは、現行関数が返す長い文面を含めて実測する。
- **a11yの実際の読み上げ**は未確認。現状ですでに明示的な行名が欠けているため、「既存の読み上げがそのまま維持される」とは扱わない。
- **5段階の視覚的な近さ表示は失われる。** 今回はオーナー要望に基づく仕様変更として明示し、本日の強調と既存の日付文面を残す。
- **GitHubのcurrent main・関連PRとの重複は未照合。** 本書は提示されたIssue内容とローカルHEADに基づく。実装着手前に同じ描画経路の変更有無を確認する。