# Issue #743 設計書（Codex GPT Sol 作成、司令塔検分済み）

位置づけ: 本書は Issue #743 の実装設計である。対象コードは v0.48.0（24a40400）。実装時は実コードを正とし、本書と食い違う場合は実コードの契約を優先して司令塔に報告すること。

**Issue #743 設計書：ファイル行の右端で日付とピンを入れ替える**

確認対象は v0.48.0 のコミット `24a40400a3fedbc3ae229e6a408a6264cd497ea4`。以下は実コードを読んだ設計であり、ファイル変更・テスト実行は行っていません。

**1. 現状の構造**

行番号は確認対象コミットに対応します。

| 出典 | 確認した構造・動作 |
|---|---|
| `crates/ui/src/view.rs:159,170,186` | サイドバー最小幅は160px、行高は24px、行の左右パディングは各4px。 |
| `crates/ui/src/view/sidebar.rs:405` の `work_folder_files_sidebar`、特に609–674行 | ファイル行は横方向の flex。子要素は `content`、`pin_slot` の順。`content` の外側に24pxのピン領域が付く。 |
| `crates/ui/src/view/sidebar.rs:617–630` | リネーム対象行は `inline_rename_label` を名前に使い、日付は `None` にする。 |
| `crates/ui/src/view/sidebar.rs:2265` の `file_row_content` | 名前領域と日付を横並びにする。名前領域は `flex_1 + min_w(0) + overflow_hidden`。インデントはその内側に置き、深い階層でも日付を押し出さない。 |
| `crates/ui/src/view/sidebar.rs:2316` の `file_name_label` | `.md` を表示上除去し、認識した日付を名前から分離する。日付のみのファイル名でも、空の名前要素を残して左側を伸縮させる。 |
| `crates/ui/src/view/sidebar.rs:2340,2351` | `date_badge_text_alpha` は本日1.0、それ以外は未来も含め0.7。`date_badge_text` は背景なし、10px、折り返しなし、`flex_none`。 |
| `crates/ui/src/view/sidebar_pins.rs:20,590` | `PIN_SLOT_SIZE=24.0`。`pin_toggle_slot` は24×24px、`flex_none`。`invisible()` でもレイアウト上の幅は残る。 |
| `crates/ui/src/view/sidebar_pins.rs:598–635` | `pinned` はアイコンとラベルを切り替える。未ピンは `ICON_PIN`／「ピン留め」、ピン済みは `ICON_PIN_OFF`／「ピン留めを解除」。**ピン済みでも非ホバー時には表示されない。** `relative=None` はボタンなし。 |
| `crates/ui/src/view/sidebar.rs:651`、`sidebar_pins.rs:615,728` | 行の `.group("sidebar-pin-row")` と子の `.group_hover(..., visible)` が対応する。ボタン上だけでなく、行へのホバーでピンを表示する。 |
| `crates/ui/src/view/sidebar_pins.rs:616,632` | ピン領域で左マウスダウンの伝播を止め、ボタンのクリックで `toggle_pin` を呼ぶ。行のファイルオープン・リネームへ流さない構成。 |
| `crates/ui/src/view/sidebar.rs:311,727–798` | フォルダ行は名前領域、新規作成48px、ピン24pxの順。作成領域も常時幅を確保し、行ホバーで表示する。ルート行は作成領域のみ。 |
| `crates/ui/src/view/sidebar_pins.rs:669–784` | 独立した「ピン留め」領域の行は、名前・親パスとピン領域で構成する。`file_name_label` を使わず、日付表示も持たない。 |

現在の日付の右端は、行パディング内側ではなく **ピンスロットの左端** です。これは `file_row_content` のコメントと既存テストにも明記されています。日付だけの余白調整ではなく、末尾の領域構成を変更する必要があります。

**2. 採用方式と状態定義**

**通常ツリーのファイル行だけに共有末尾領域を設け、日付とピンをホバーで入れ替えます。**

```text
現在
[ インデント・アイコン・名前 … ][ 日付 ][ ピン用24px ][右padding]

変更後・非ホバー
[ インデント・アイコン・名前 … ][         日付 ][右padding]

変更後・ホバー
[ インデント・アイコン・名前 … ][       ピン解除/ピン ][右padding]
                                  ↑同じ領域
```

共有領域内では日付を通常フローに置き、既存ピンスロットを右端に絶対配置します。これは「同じ右端領域で入れ替える」方式です。日付を表示したままピンで覆う方式にはしません。

採用理由は次のとおりです。

- 非ホバー時の日付を、行の右パディング内側まで移動できる。
- 日付とピンの同時描画を避け、背景色による遮蔽が不要になる。
- ホバーで幅を変えないため、名前の省略位置と日付の座標が動かない。
- ピンのボタン生成、24pxの操作領域、イベント処理を再利用できる。
- 同じリポジトリには `entry_icon` のアイコンと開閉矢印を入れ替える類似構造がある（`sidebar.rs:2223–2253`）。

**レイアウト契約**

共有領域を以下に固定します。

- 高さ：`PIN_SLOT_SIZE` の24px。
- 幅：`max(日付の自然幅, PIN_SLOT_SIZE)`。日付なしは24px。
- `relative + flex_none`。内部は右寄せ・上下中央揃え。
- 日付は通常フローに残す。ホバー時は `invisible()` にし、幅を維持する。
- ピンは幅・高さ24pxのラッパー内に置き、共有領域の `right=0, top=0` に絶対配置する。
- 名前領域と共有領域の間は、既存 `file_row_content` の `gap_1` を維持する。
- 共有領域そのものにはクリックハンドラーを付けない。
- 非ホバー時は、`date.right = row.right − 4px`。
- ホバー時は、`pin.right = row.right − 4px`。

既存 `pin_toggle_slot` はそのまま共有領域内へ移します。テスト時には同サイズのラッパーが追加されるため、**返された要素自身に絶対配置を付けず、呼び出し側で絶対配置ラッパーを作る**こととします。これにより通常ビルドとテストビルドの配置差を避けます。

| 対象状態 | 非ホバー | 行ホバー中 | 操作・補足 |
|---|---|---|---|
| 未ピン・日付あり | 日付を右端表示。ピン非表示 | 日付非表示、右端にピン表示 | ピンで留める。名前の位置・省略幅は不変 |
| ピン済み・日付ありの通常ツリー行 | 日付を右端表示。解除ボタン非表示 | 日付非表示、右端に解除ボタン表示 | 解除できる。ピン済みを理由に常時表示へ変更しない |
| 日付なし・未ピン／ピン済み | 24pxの空領域 | ピン／解除ボタン表示 | 名前との重なりを防ぐ |
| リネーム対象行 | 日付なし、入力欄と24pxの空領域 | 日付なし、既存どおりピン／解除ボタン表示 | ピン操作は既存 `pin_interaction_block` が拒否。入力内容を保持 |
| `pin_relative=None`・日付あり | 日付を右端表示 | 日付を表示し続ける | 操作できるピンがないため日付を隠さない |
| 独立した「ピン留め」領域の行 | 既存の名前・親パス表示 | 既存の解除ボタン表示 | 共有領域への置換対象外 |
| フォルダ行・ルート行 | 現状維持 | 現状維持 | 新規作成領域の幅・表示条件を維持 |

リネーム終了後は、確定後のファイル名または取消後のファイル名から日付を再構築します。終了時に行がホバーされていればピン、非ホバーなら日付を表示します。

**クリック仕様も明示します。** ホバー中の末尾24pxはピン操作領域です。「本日」のように短い日付が表示されていた位置をクリックすると、ポインター進入後はピン操作になります。共有領域のうち24pxより左側の余白と名前領域は、行の通常クリックに従います。

**3. ファイル・関数単位の作業分割**

| ファイル・関数 | 作業内容 |
|---|---|
| `crates/ui/src/view/sidebar.rs`：新規 `file_row_trailing_slot` | `row_index`、`date: Option<Div>`、既存の `pin_slot: Div`、`can_toggle_pin: bool` を受け取り、上記の共有領域を構築する。行ごとの測定用 selector を付ける |
| 同：`file_row_content` | 最終引数を `date: Option<Div>` から共有末尾要素 `trailing: Div` に変更。名前領域の伸縮・クリップ・内側インデントを維持し、その後ろに共有要素を配置する |
| 同：`file_name_label` | 既存の日付抽出・名前生成を維持。ピンスロットと操作可否を受け取り、日付の有無にかかわらず `file_row_trailing_slot` を経由して行内容を返す |
| 同：`work_folder_files_sidebar` のファイル分岐 | `pin_relative`、`pinned`、`can_toggle_pin = pin_relative.is_some()`、`pin_slot` を内容構築より先に生成。リネーム分岐では日付なしの共有領域を渡す。外側行の `.child(pin_slot)` を削除する |
| 同：`date_badge_text` | 日付文字列、10px、文字色・濃度の計算は維持。必要ならテスト専用の描画確認用子要素を追加する |
| 同：テスト群 | 旧ピンスロット左端基準の検証と、日付座標をクリックするテストを更新する |
| `crates/ui/src/view/sidebar_pins.rs` | `pin_toggle_slot` の動作は維持。コメントを、ファイル行では共有領域内にも置かれる説明に更新する。既存テストを回帰確認する |

`sidebar-pin-row` のグループ名を維持し、ホバー状態をモデルへ追加しません。ピンの永続化、並び順、保存中の制御も今回の変更対象に含めません。

**4. テスト計画**

既存の `date_sidebar_view`、`file_row_index`、`row_selector`、`pin_selector` を再利用します。ピン追加後は独立領域の出現で位置が変わるため、対象行と bounds を取り直します。

**既存テストへの影響**

| 既存テスト・ヘルパー | 更新方針 |
|---|---|
| `assert_date_aligned_to_pin_slot` | `assert_date_aligned_to_row_right_edge` に置換。行ごとの外側 bounds を取得し、日付右端と `row.right − SIDEBAR_ROW_HORIZONTAL_PADDING` を比較する |
| `date_badge_text_aligns_to_the_content_right_edge` | 行パディング内側との一致まで検証する名前・内容に変更。全行で同じ右端になることも維持 |
| `date_only_file_name_keeps_the_date_right_aligned` | 新しい右端基準へ変更。空の名前領域を残す検証は維持 |
| `long_and_deep_file_names_do_not_clip_date_badge_text` | 最小幅・12階層・長い日本語名で、新しい右端、日付幅、名前との非重複を検証 |
| `pin_hover_does_not_move_date_badge_text` | 座標不変に加え、日付とピンの表示切替・退出時の復元を検証するテストへ変更 |
| `inline_rename_hides_and_restores_date_badge_text` | 開始前の選択を日付座標から名前領域へ変更。復元確認前にはポインターを行外へ移す |
| `clicking_date_badge_text_opens_its_file_row` | 旧クリック仕様を維持しない。名前クリックで開く検証と、末尾24pxでピンを操作する検証に置換 |
| `date_badge_text_uses_full_opacity_only_for_today`、`date_badge_text_contrast_is_readable_on_both_sidebar_backgrounds` | 維持して実行 |
| `sidebar_pins` のクリック、長い名前、保存・解除関連テスト | 維持して回帰実行。特に `clicking_the_toggle_pins_once_without_opening_the_row` と `a_pin_row_with_a_long_name_and_parent_stays_on_one_row` を確認 |
| フォルダ作成ボタン関連テスト | 最小幅での操作到達性、行クリックへの非伝播、ツールチップ表示を維持 |

**追加・置換するテスト**

| テスト名 | 検証内容 |
|---|---|
| `date_badge_text_aligns_to_row_padding_edge` | 「本日」と長い過去年表記で、非ホバー時の右端が行パディング内側に一致する。許容誤差0.5px |
| `file_row_hover_swaps_date_and_pin_without_relayout` | 行外→名前上→ピン上→行外と移動。名前・共有領域・日付の bounds は不変。日付とピンの描画は排他的 |
| `pinned_file_row_restores_date_after_hover` | 留める／解除する操作後も、通常ツリー行の非ホバー日付が右端へ戻る。解除ボタンが機能する |
| `file_row_without_date_keeps_pin_reachable` | 日付なしで共有領域が24pxを確保し、ピン操作がファイルを開かず一度だけ反映される |
| `file_row_trailing_pin_click_does_not_open_or_rename` | 日付あり行の末尾24pxをクリック・ダブルクリックしても、ファイルオープンやリネームを開始しない |
| `file_row_name_click_opens_file_with_shared_trailing_slot` | 名前領域のクリックは従来どおりファイルを開く |
| `file_date_stays_visible_when_pin_target_is_unavailable` | `can_toggle_pin=false` ではホバーしても日付を消さない。ヘルパーを使った描画 fixture で確認 |
| `inline_rename_keeps_shared_pin_slot_and_blocks_toggle` | リネーム中の日付非表示、24px確保、ピン操作拒否、入力内容保持、取消・確定後の復元を確認 |

**レイアウト・描画・GUIの分担**

`debug_bounds` では、座標、幅、行高、領域同士の重なり、ホバー前後の不変性を固定できます。

ただし、**bounds が存在することは表示されている証拠になりません。** 既存 `keyed_pin_slot_for_test` のコメントにも、非表示スロットの bounds が取得できるとあります。

表示切替の自動検証には、非表示になる親要素の**子孫**にテスト専用 selector を置きます。親自身の selector を可視性判定に使いません。日付側には寸法へ影響しない描画確認用子要素を追加し、ピン側は既存のボタン用子孫 selector を利用します。GPUIでは非表示要素自身の bounds 登録後に子の描画を打ち切るため、この差を利用できます。

実画面では以下を確認します。

- 標準幅／最小幅、長い名前／深い階層での日付の右揃え。
- 行への進入・退出、行間移動でのちらつき、重複描画、名前の揺れ。
- 本日・過去・未来、通常背景・選択背景での10px文字と濃度。
- ピン留め済み通常行、独立したピン領域、フォルダ作成ボタンの表示。
- リネーム開始・取消・確定とピン操作領域の干渉。

実装後は対象headの Debug／development build を使用し、SHA・ビルド種別・実行バイナリの由来を記録します。対象テストと `cargo fmt --all -- --check` を別々に実行し、その後 `hane-ui` の関連テストを確認します。

**5. 受入条件チェックリスト**

- [ ] **日付の完全右寄せ**：非ホバー時、日付の右端が行右端から4px内側に一致し、24pxの余白が残らない。
- [ ] **全日付表記への適用**：「本日」、長い年付き表記、日付のみの名前でも同じ基準へ揃う。
- [ ] **ホバーとの両立**：行ホバー中は日付を隠してピンを表示し、重複描画しない。退出すると復元する。
- [ ] **配置の安定**：ホバーの切替で名前の省略位置、共有領域の幅、日付の座標が動かない。
- [ ] **ピン留め済み行の維持**：通常ツリー行で解除でき、非ホバー時には日付が戻る。独立したピン領域の名前・親パス・解除操作も維持する。
- [ ] **Issue #719 の維持**：背景なし10px、本日は濃度1.0、それ以外は未来も含め0.7。日付抽出・相対表記を維持する。
- [ ] **日付なし・リネーム対応**：日付なしでもピンを操作でき、リネーム中は日付を出さず入力を保護する。取消・確定後は状態表どおり表示する。
- [ ] **イベントの整合**：ピン操作がファイルオープン・リネームへ伝播せず、名前クリックは従来どおり動作する。
- [ ] **狭い幅・深い階層**：最小幅160pxでも日付とピンが行内に収まり、名前が先に縮む。
- [ ] **フォルダ行の回帰防止**：作成48px＋ピン24pxの既存配置・ホバー表示・操作を維持する。