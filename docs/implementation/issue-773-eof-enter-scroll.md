# Issue #773 設計書 — 文書末尾の Enter で表示が逆方向に跳ねる

対象は `origin/main` の `8b27e740`。修正方針は、末尾の改行で表示内容が変わらないブロックの実測高さを、同期 splice と正式索引の公開をまたいで保持すること。新しい空行への追随は、描画に使うレイアウトで確定する。

コードからは、実測高さを失う箇所と、その縮んだ索引でスクロール位置を確定する箇所を特定できた。papa の画素記録だけでは、観測された **104px をどのブロック・どのフレームの高さ差が作ったか**までは特定できない。以下の計測で帰属を確定し、再現した経路に修正を絞る。

## 1. 原因と仮説の検証

### README の Enter は同じブロック数でも高さを置換する

対象の README は 24,850 バイトで末尾に LF がある。Cmd+Down は EOF、つまり 215 行目の文章の後にある空行へ移動する。本文の Enter はそこに LF を一つ追加する。最後の空行は引き続き最終段落が所有し、Markdown ブロック数は変わらない。最終段落の高さ見積もりは、文章の物理行と EOF の空行で `2L`、Enter 後は空行が一つ増えて `3L` になる。`L` は本文の行ピッチ。末尾の空行はすでに高さ計算に含まれている。 [README.md](README.md#L209) · [actions.rs](crates/ui/src/actions.rs#L384) · [editor/lib.rs](crates/editor/src/lib.rs#L301) · [block_index.rs](crates/markdown/src/block_index.rs#L872) · [projection/block.rs](crates/presentation/src/projection/block.rs#L94)

1. **同期 splice。** 正式索引が入った状態では `after_input` → `BlockIndexState::apply_edits` → `apply_height_index_update` を通る。EOF 編集の再解析窓には最終段落とその直前の段落が入る。両方のブロックに新しい解析リビジョンが付き、`range_eq` は成立しない。`height_sync.rs:703` でこの窓の実測値を推定値に置き換え、同 `:721` で上端位置を再計算する。窓より前の高さは残る。上端ブロックが置換窓より前にあれば、その時点では `scroll_y` が同じでも文書高は縮み得る。この入力で同期の `resync_heights` に落ちるには、索引未構築など splice の成立条件を満たさない事情が必要になる。 [view.rs](crates/ui/src/view.rs#L2703) · [block_index.rs](crates/markdown/src/block_index.rs#L1208) · [block_index.rs](crates/markdown/src/block_index.rs#L1464) · [height_sync.rs](crates/ui/src/view/height_sync.rs#L635) · [height_sync.rs](crates/ui/src/view/height_sync.rs#L694)
2. **非同期の正式公開。** 現リビジョンの正式パースが公開されると、`background_parse.rs:232` から `install_heights` に入る。正式パースは編集位置より前のブロックにも新しいリビジョンを付ける。`preserve_measured_block_heights` はリビジョンまで一致するものを引き継ぐため、同期 splice の窓外に残っていた折り返しの実測は、バイト範囲と推定値が同じでも落ちる。`height_sync.rs:616–623` はその索引で上端の source offset とブロック内位置を復元する。上端より前の高さ減少は prefix sum を縮め、上端ブロック自体の縮小は `intra` を切り詰め得る。 [background_parse.rs](crates/ui/src/view/background_parse.rs#L198) · [block_index.rs](crates/markdown/src/block_index.rs#L944) · [view.rs](crates/ui/src/view.rs#L1549) · [height_sync.rs](crates/ui/src/view/height_sync.rs#L574)
3. **クランプとキャレット補正。** `render.rs:134` は可視範囲を選ぶ前に、実測と推定が混じる `HeightIndex` の合計でクランプする。実測更新後のアンカーは、このクランプを通過した位置を基準にする。そのため、後から文書高が回復しても、上限が縮んだ時に捨てた位置は自動では戻らない。入力直後のキャレット追随は、現在リビジョンのキャッシュがなければ物理行ベースに落ちる。ただし同じ Enter の描画で実測のキャレット行が得られれば、`render.rs:275` で補正できる。**次の Enter が必要という仮説が成立するのは、正式公開が補正要求を解除した後に位置を崩す場合、または補正に必要な行・フレームが得られない場合。** `cx.notify()` のみの分岐は、描画中に実フレームを要求する既存ヘルパーとは挙動が異なる。 [render.rs](crates/ui/src/view/render.rs#L134) · [render.rs](crates/ui/src/view/render.rs#L184) · [render.rs](crates/ui/src/view/render.rs#L243) · [view.rs](crates/ui/src/view.rs#L4907) · [viewport.rs](crates/ui/src/view/viewport.rs#L180)

### 104px の計算と帰属

上限は `max(0, H + B − V)`。`H` は高さ索引の合計、`B` はバッジの 11pt、`V` は本文ビューポート高。幅・倍率・ビューポートが一定なら、前後の上限差から `B` と `V` は消える。末尾の空行は `+L`。同期 splice の推定への置換で折り返し等の上乗せを `E` 失えば、索引の差は `L − E` になる。たとえば、入力前が上限、同期アンカーが不動、かつ `E = 3L` なら、`render.rs:134` が `2L` を切り詰める。52px ピッチでは 104px に相当する。この計算は**最初のクランプでの損失**を説明する。安定後にも残る量には、再計測後のアンカーとキャレット補正が関わる。README の末尾の再解析窓は段落二つなので、その窓でフェンスの畳み行が増減することはない。 [view.rs](crates/ui/src/view.rs#L314) · [viewport.rs](crates/ui/src/view/viewport.rs#L4) · [line.rs](crates/ui/src/line.rs#L2000) · [README.md](README.md#L209)

さらに、静止画で追う同じ文字列の位置は `行の文書内 y − scroll_y`。その行より前の高さも再計測で変わるため、本文の `+104px` から内部 `scroll_y` の差を直接逆算できるのは、計測対象行の文書内 y が一定の場合に限る。2 回目の `−156px` は「既存の 2 行分のずれを解消し、新しい 1 行分を追随する」と整合する。同期 splice、正式公開、前段クランプのどれが安定後のずれを残したかは、フレーム単位で照合する。 [render.rs](crates/ui/src/view/render.rs#L422) · [render.rs](crates/ui/src/view/render.rs#L512)

## 2. Claude Code の実装フェーズ

### フェーズ A — 修正前に損失を計測して回帰を固定

1 回目の Enter の直前から、正式公開後の安定フレームまでを採取する。`scroll_y` が初めて減る箇所と、キャレットが外れたままになる箇所をそれぞれ特定する。各書き込みの前後で、文書リビジョン、索引の公開種別、splice の範囲・件数、置換ブロックの旧高さ・推定・再計測値、上端のブロックと `intra`、`H`、`V`、`scroll_y` を記録する。描画時は `fresh_caret` の有無・上下端、追随フラグ、画素比較に使う行の文書内 y も記録する。最初の描画と 40ms の正式パース待ちを分け、Enter を追加せずに安定まで追う。診断出力は作業用とし、PR には再現する回帰テストを残す。 [height_sync.rs](crates/ui/src/view/height_sync.rs#L697) · [background_parse.rs](crates/ui/src/view/background_parse.rs#L79) · [render.rs](crates/ui/src/view/render.rs#L134) · [render.rs](crates/ui/src/view/render.rs#L249)

### フェーズ B — 高さを失う経路を修正

- **同期 splice で損失が再現する場合**は、再解析窓の中でも編集と表示上の影響を受けないブロックの高さを引き継いでから splice し、その索引で上端を復元する。初回の対象は、末尾空行への LF 追加で範囲・構造・表示が変わらない直前の段落。最終段落を推定に戻すことで同じ描画に逆方向の移動が残る場合は、現リビジョンで描画する範囲を再計測してから位置を確定する。旧高さに行高を足す最適化を採る場合は、旧行の折り返し・開示・描画範囲が同じで増分が空行一つと確認できる場合に限る。 [height_sync.rs](crates/ui/src/view/height_sync.rs#L661) · [height_sync.rs](crates/ui/src/view/height_sync.rs#L697) · [render.rs](crates/ui/src/view/render.rs#L184)
- **正式公開で損失が再現する場合**は、公開前の索引と編集差分を使い、同じ表示内容と確認できるブロックの高さを推定索引に移してからアンカーを復元する。初回の対象は末尾空行への LF 追加。追記位置より前の同一ブロックについて、同じ文書・幅・フォント・モードで、ID・範囲・種別・行数・推定値と、開示など高さを変える条件の一致を確認する。`RevisionDelta` の変更範囲と追記されたバイトを確認し、途中に別編集がある場合や根拠が欠ける場合は既存判定を使う。リビジョン一致条件を全体から外すと、同じ範囲に別の文字列を入れた編集まで旧高さを再利用してしまう。全文走査は既存の正式公開側に収め、同期入力側は splice 窓内で完結させる。 [document/lib.rs](crates/document/src/lib.rs#L107) · [view.rs](crates/ui/src/view.rs#L1549) · [background_parse.rs](crates/ui/src/view/background_parse.rs#L156) · [height_sync.rs](crates/ui/src/view/height_sync.rs#L605)
- **描画側の追加修正は、実測保持後にもキャレット欠落が再現する場合に限定する。** 末尾編集の進行中に限り、現リビジョンの EOF 行を描画範囲に含め、実測更新後・描画位置確定前に既存のキャレット補正を通す。次フレームが必要なら `request_layout_followup_frame` を使い、実測で収束したら終了する。正式公開のたびに追随を再起動すると、Enter 後にホイールなどで別の場所へ移動した利用者を EOF に引き戻す。要求を追加するなら同じ文書・編集リビジョン・キャレットに結び付け、明示的なスクロールやジャンプで解除する。 [render.rs](crates/ui/src/view/render.rs#L254) · [viewport.rs](crates/ui/src/view/viewport.rs#L180) · [view.rs](crates/ui/src/view.rs#L4475)

### フェーズ C — 既存契約を検証

ADR-0020 の物理行ベースの初期高さ、ADR-0021 の `BlockLayout` による実測、ADR-0022 の局所 splice と上端アンカーを使う。トレードオフは、表示内容の等価性を確認できる範囲に実測の引き継ぎを絞ること。別条件の高さを引き継いで一時的に行を隠すリスクを避け、今回の同期入力コストも局所に収める。共通クランプは、実際に文書が縮んだ場合、バッジの余白、ホイール慣性の端点に必要。中央寄せは固定描画窓、文書内検索は専用の追随要求を持つ。これらの優先順位を変えず、既存のホイール・中央寄せ・検索ジャンプのテストを通す。 [ADR-0020](docs/adr/0020-block-virtualization.md#L68) · [ADR-0021](docs/adr/0021-layout-lines-and-visual-coordinates.md#L89) · [ADR-0022](docs/adr/0022-layout-cache-invalidation.md#L38) · [basic_editing_keys.rs](crates/ui/src/view/basic_editing_keys.rs#L832) · [basic_editing_keys.rs](crates/ui/src/view/basic_editing_keys.rs#L1025) · [document_find.rs](crates/ui/src/view/document_find.rs#L662)

## 3. 受入テスト

`crates/ui/src/view.rs` に `#[gpui::test] fn eof_enter_keeps_the_caret_visible_in_a_wrapped_document(...)` を追加する。実ウィンドウのテストビューでキー割り当てを登録し、狭いウィンドウにリサイズして本文にフォーカスする。fixture は LF 終端で、短い最終段落、その直前に折り返す段落、さらにビューポートより上にも折り返す段落を含める。先頭を描画して高さを計測してから、macOS では `cmd-down`、ほかの対応プラットフォームでは `ctrl-end` を送り、EOF を描画する。テストの事前条件として、`Granularity::Blocks`、上方のブロックで実測高が推定を超えること、初期キャレットが可視であることを確認する。 [view.rs](crates/ui/src/view.rs#L10246) · [view.rs](crates/ui/src/view.rs#L21489) · [view.rs](crates/ui/src/view.rs#L24740) · [actions.rs](crates/ui/src/actions.rs#L152)

`simulate_keystrokes("enter")` を一回ずつ送り、各回で **最初の描画**と **正式公開後の安定描画**を検査する。`run_until_parked` とテスト時計の進行を分け、安定側では当該リビジョンの正式索引が実際に公開されたことを確認する。各観測点で EOF に LF が一つだけ増え、キャレットは新しい EOF にあり、`caret.height > 0`、`caret.y >= −ε`、`caret.y + caret.height + CARET_MODE_BADGE_HEIGHT <= viewport_height + ε` を満たす。固定幅・固定倍率の fixture で、Enter 前からの `scroll_y` の差は `−ε` 以上、`line_height + ε` 以下。本文の固定行も併せて追跡し、prefix sum の変化で内部値だけが改善したように見えるケースを検出する。安定後は追加キーを送らず、さらに描画しても位置がずれないことを確認する。幅 960pt の折り返しなしと通常文字入力を対照にする。 [view.rs](crates/ui/src/view.rs#L10316) · [render.rs](crates/ui/src/view/render.rs#L422)

既存の「編集されたブロックの旧高さを捨てる」「幅が変わったら捨てる」「縮んだ文書の末尾を越えない」「EOF のバッジを見せる」の期待値は維持する。窓内の**全**挿入値を推定値と比較している `incremental_height_splice_preserves_measured_height_outside_the_update` は、同期 splice の保持範囲を広げた場合のみ、変更された項目は推定値、等価な項目は実測値という検査に更新する。従来の fixture で窓内の値がすべて推定値のままなら、既存テストに加えて窓内保持のケースを足す。 [view.rs](crates/ui/src/view.rs#L14200) · [view.rs](crates/ui/src/view.rs#L14271) · [view.rs](crates/ui/src/view.rs#L14017) · [view.rs](crates/ui/src/view.rs#L14073) · [view.rs](crates/ui/src/view.rs#L10590)

## 4. papa の GUI 検証

1. `8b27e740` と修正候補の Debug ビルドを、それぞれ隔離 state と同一バイトの README コピーで起動する。ビルド元 SHA、実PID、バイナリの由来、README のハッシュ、ウィンドウ実寸・本文幅・本文の高さ、倍率、画面スケールを記録する。幅 560pt、元計測と同じ設定で冒頭の折り返しと 52px の行ピッチを確認する。 [agent-desktop-gui-testing.md](docs/agent-desktop-gui-testing.md#L18) · [ADR-0024](docs/adr/0024-local-gui-validation.md#L25)
2. Cmd+Down 後の安定画面から録画し、Enter を一回押す。直後から正式公開・描画が安定するまで待ち、次の Enter を押す。これを複数回繰り返す。本文中の未編集で複数フレームに共通する領域を切り出し、キャレット・バッジ・スクロールバーを除いて垂直方向の相互相関を取る。入力直前、最初の変化、安定後のフレームを保存し、本文下向きを正として変位を記録する。キャレットの上下端は各可視フレームで別に確認する。
3. 基準ビルドで `+104px` と `−156px` の再現を撮り、フェーズ A のログと時刻を合わせる。修正候補では移動不要な間は `0px`、既存の余白を使い切った後は `−52px/回` を確認する。余白を使い切る回に端数があれば `0` から `−52px` の間。**正の変位、1 行を超える移動、キャレットの画面外への脱落は、途中フレームでも不合格。** 別の新規コピーで通常文字 `0px` と幅 960pt の対照を確認する。画面取得や入力が失敗した場合は失敗した層を切り分け、復旧後に同じシナリオを再実行する。 [gui-test-recovery.md](docs/gui-test-recovery.md#L8)

## 対象外と確認状況

高さ推定そのものの精度改善、仮想化ウィンドウの再設計、リスト編集規則、フェンスやバッジの寸法変更は対象外。今回の変更は、末尾の改行に伴う高さの引き継ぎとキャレット可視性に限る。

読み取り専用で設計を作成し、リポジトリは変更していない。GPUI テストと papa の追加計測は未実施。正確な 104px の内訳はフェーズ A の確認項目として残る。Jev の照会はローカル CLI が設定指定の `typesafe` を受け付けず、設計判断は対象コードと提示された計測結果に基づく。
## 司令塔の補足（2026-11-25）

- Jev の判断: 等価性条件で限定した実測高さの引き継ぎ緩和は、同期 splice と正式公開の両経路で許容される（choice: equivalence_scoped_both_paths、confidence 1.0）。適用はフェーズ A の計測で損失が確認された経路に限る
- 実装の分割: ハンドオフ1 = フェーズ A（損失経路の特定＋回帰テスト `eof_enter_keeps_the_caret_visible_in_a_wrapped_document` の追加。現行コードで失敗することを確認し、帰属結果をこの文書の末尾に追記する）。ハンドオフ2 = フェーズ B（該当経路の修正）。フェーズ C の既存契約確認は B の検証に含める
- 現時点の状態: 未実装・未検証。設計のみ

## フェーズ A 帰属結果

ワーカーにはシェル・テスト実行権限がないため、以下は**コードの静的読解に基づく帰属**であり、実測値ではない。回帰テスト `eof_enter_keeps_the_caret_visible_in_a_wrapped_document`（対照: `eof_enter_and_typing_keep_the_caret_visible_without_wrapping`）は `crates/ui/src/view.rs` に追加済みで、各 Enter の「first」「settled」「stable」の各段階で scroll_y・キャレットの y と高さ・viewport_height を失敗メッセージに出す。司令塔の focused テスト実行の出力で、下の仮の帰属を確定または訂正する。

- **scroll_y が初めて減る時点**: 同期 splice（`apply_height_index_update`）は上端ブロックが置換窓より前にあるとき `scroll_y` を再計算するが、窓の外の高さは残るため、そこでは `scroll_y` は動かない。減る最初の箇所は、同じ Enter の描画で `render.rs:134` のクランプが、窓内の実測を失って縮んだ `H` の上限へ `scroll_y` を切り詰める点と読める。テストの `first` 段階がこれを捕らえる。正式公開（`install_heights`）は次の二次的な縮みを加え得る（`settled` 段階）。
- **キャレットが外れたままになる主体**: 描画側のクランプ（`render.rs:134`）が捨てた位置は、後で `H` が実測で回復しても戻らない。キャレット補正（`render.rs:254` 付近）は現リビジョンのキャッシュがないと物理行ベースに落ちる。したがって損失の主体は「高さ索引の縮み」であり、描画側は結果を確定する役である。
- **104px（2行分）の内訳として確認できた高さ差**: 設計書 §1 の式（`L − E` と切り詰め `2L`）は、最終段落と直前の折り返し段落が窓内で実測→推定に置換されることを前提にする。このテストの fixture では直前の折り返し段落（10 回反復の段落）がこの窓に入る。ブロックごとの実測→推定の具体的な差分は未計測で、司令塔のテスト出力（scroll_y の前後値とキャレット位置）で照合する。
- **フェーズ B で修正すべき経路の結論（暫定）**: 第一に同期 splice（`height_sync.rs:703` の窓内置換。編集と表示が変わらない直前の段落の実測を引き継ぐ）。第二に、正式公開側（`preserve_measured_block_heights` がリビジョン違いで実測を落とす点）。いずれも、`first` 段階で失敗するなら同期 splice、`first` が通り `settled` だけ失敗するなら正式公開のみを修正対象にする。描画側の追加修正は、実測保持後もキャレット欠落が再現した場合に限る。
