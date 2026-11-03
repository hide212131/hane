# Issue #670 外部リンク起動不達の調査・実装準備

- 契約の正本: https://github.com/hide212131/hane/issues/670
- 調査・分岐基準main: `1e0d345e46d8c1805659968f8fad1148b0ae37a8`
- 実装用branch: `codex/issue-670-external-link-browser`
- 関連: #502 / PR #503、#603 / PR #604、#614 / #615、#598、#629 / PR #630

このcommitは文書だけの準備で、製品コード・テストは変更しない。ビルド、テスト、GUI再現は未実施。利用者は初回報告を「WindowsのCtrl＋クリックのみ」に訂正し、Markdown形式のリンクでも開かないと回答した。macOS・閲覧通常クリックの不達報告は撤回されており、両OS×両モードは回帰確認の範囲として扱う。このSHA上のGUI再現確認ではない。Windowsのバージョン・配布形態、具体的なMarkdown原文・既定ブラウザは未取得。追加要望の裸URL自動リンクは #673 と別PRに分離し、本PRへ混ぜない。

## current mainで読解した既存経路

以下は静的なコード読解であり、イベントが実機で通過したという証拠ではない。

1. `crates/markdown/src/parser.rs` の `build_tree` は `LinkType::Inline / Reference / Collapsed / Shortcut` と `is_external_http_url` を通るリンクを `LinkParse` にする。裸URL・autolinkを同等の対象と仮定しない。まず報告例を照合する。
2. `crates/markdown/src/syntax.rs` の `LinkParse`、`crates/presentation/src/display.rs` の `LinkTarget` と `VisualLine.links` がsource範囲とdestinationを保持する。表示座標からURLを推測したり、UIで別途Markdownを再parseしたりしない。
3. `crates/ui/src/view.rs` の `row_hit_at_window_point` / `row_hit_at_point` は既存layout/source mappingからcaret用offsetとリンク文字用glyph offsetを区別する。`link_destination_at` はpresentationのsource範囲に照合する。行所有者、cache、折返しfragment、zoom、scroll、検索欄の高さもこの既存変換に従う。
4. `on_row_mouse_down` → `apply_row_click` はEditingでmacOS Command / Windows Ctrl、Readingでは無修飾左クリックも `OpenLink` とする。`is_link_open_modifier` はShift/Altと他OS用修飾キーの併用を除外する。閲覧無修飾clickは `pending_reading_link` に保持して `on_panel_mouse_up` で確定。`promote_pending_link_on_move` / `link_press_is_drag` は4論理px以上を選択dragへ切り替え、MouseMoveなしreleaseでも判定する。
5. 起動は同経路の `cx.open_url`。workspaceのGPUIは `gpui-pre = 0.3.6`、`Cargo.toml` のpatch先は **`vendor/gpui-pre-macos` / `vendor/gpui-pre-windows`**。macOSの `src/platform.rs::open_url` はNSURLとNSWorkspace、Windows側はbackground taskから `open_target` / `ShellExecuteW` へ渡す。**`vendor/gpui/src/platform/...` は当該OSのactive backendと同一と仮定しない。**

## 診断と修正の境界

原因分類は現段階で `unknown`。PR担当はcurrent main・PR head・共有GUIの利用状況と利用者回答を読み直す。最初から製品修正を依頼せず、必要ならCommander Policy §4.7のread-only診断契約を選ぶ。`unknown` のまま `product` や `initial` として通常実装に送らない。

最小fixtureは `[外部リンク](https://example.com/hane-link-probe?case=CASE)` とreference link。検証ごとの一意URLを用い、必要なら自分が管理するローカルHTTP endpointを追加する。HTTP requestだけで入力経路や画面到達を断定しない。利用者文書や既存ブラウザ窓を実験用に変更しない。

観測は入力配送、Hane受信のbutton/modifierと座標、実glyph hit、destination、`cx.open_url`、active OS backendへの到達、ブラウザのURLを分ける。座標は表示倍率と論理座標の対応を記録する。計測が必要な場合は隔離した候補に限定し、計測の存在だけで製品原因確定としない。OSの既定HTTP/HTTPS関連付けはread-onlyで確認し、通常のOS起動で同じURLを開く対照を取り、製品と環境を切り分ける。ブラウザが既に起動している場合は新規processの有無だけで判断しない。

原因を示す観測と「どの観測が修正後に変わるか」が揃ってから、同じroot causeをまとめて通常のClaude実装経路へ渡す。今回の受付チャットはClaude依頼を投稿しない。PR担当チャットがexact-head・権限を確認し、必要な診断、実装依頼、prepare受理確認以降を担当する。

新しいリンク機能、カーソル案内、相対リンクrouting、設定、WebView、全域refactorは混ぜない。既存契約を維持する局所修正を想定し、新ADRは作らない。診断により複数Issueへ影響する設計変更が必要と分かった場合だけ、関連ADRと保護契約を先に整理し、範囲をIssueへ反映する。

## focused検証の実施条件

受入条件・両OS×両モードの期待操作はIssue本文を正本とする。各セルにOS/Hane version、配布形態、exact head/base、実行パス・PID・binary hash、隔離state、Markdown原文、mode、button/modifiers、press/release論理座標、期待destination、観測、artifactを記録する。

- macOS Editing: 通常clickは選択、Command＋clickは外部起動。Windows Editing: 通常clickは選択、Ctrl＋clickは外部起動。
- 両OS Reading: 通常clickでmouse-up時に外部起動、既存Command/Ctrl＋clickも維持。4論理px未満・ちょうど4・超過、MouseMoveなしreleaseを区別する。
- inline/reference、soft-wrap、複数行、日本語label、末尾glyph、表示とsourceが一致しない記法、scroll/zoom後を原因に応じてfocusedに選ぶ。negativeは非リンク・画像・相対・file・未対応schemeとShift/Alt併用。
- 起動でcaret/selection/bytes/revision/Undo/Redoが不変であることはstateの回帰テストとGUI前後観測を組み合わせる。保存ファイルhashだけで未保存本文や履歴の不変を証明しない。

既存の `link_open_modifier_is_command_on_macos_and_ctrl_elsewhere`、`link_open_click_leaves_editor_state_untouched_and_other_clicks_edit`、`reading_link_click_opens_on_mouse_up_and_drag_selects_through_dispatch`、`reading_link_release_without_move_uses_threshold_through_dispatch`、reference定義変更後のdestination鮮度テストを起点とし、原因に合う最小回帰を選ぶ。既存テストの存在を今回の実行成功と書かない。CI、review、GUI evidenceはその時点のcurrent head/baseとの対応を再確認する。

Windows GUIは利用者が許可した既存WindowsチャットからWindows上のChatGPTアプリで直接操作する。MacからVMを操作しない。GUI失敗時は `docs/gui-test-recovery.md` と `hane-gui-test` skillを読み、観測層を診断して復旧する。所有する候補だけをPID/pathで確認して停止し、終了確認後にbundle/stateを片付ける。unknownやblockedを受入済みにしない。PR #604の過去のGUI例外承認は今回へ移さない。

## 依存・引継ぎ

調査時open PRとin-progress runはゼロ。#297/RF0全体の完了は不要で、受入済みのクリックとOS起動経路を使える。#598の相対リンク、#614/#615のcursor、#567のview分割など、着手時に同じ責務を変える作業を再確認する。共有checkoutの `crates/ui/src/view.rs` に既存の未commit変更があるため、受付では `/tmp/hane-url-link-intake-20261104` の独立cloneを使った。共有変更をcheckout/reset/stashで上書きしない。

Jev: 公開済みfactsのみでprovider `typesafe`、要求model `jev-latest`、応答model `jev-1.14.0` を使用。新規bug契約の分離は `create_bug`（confidence 0.98）、文書準備は `prepare`（0.88）を選択。実行ログは受付チャットにあり、意味判断は実機再現や受入の証拠ではない。

利用者の訂正後、Jevは公開コードの既存契約から既存起動不達と裸URL新対応を `split`（confidence 0.98）と判断。受付は #670 と #673 を別Issue/PRに分離した。
