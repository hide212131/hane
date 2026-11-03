# Issue #673 裸HTTP(S) URLのリンク対応・実装準備

- 契約: https://github.com/hide212131/hane/issues/673
- 基準main: `1e0d345e46d8c1805659968f8fad1148b0ae37a8`
- 実装用branch: `codex/issue-673-bare-http-url`
- 関連: #502 / PR #503、#603 / PR #604、#670 / PR #674、#97

これは文書だけの準備commitであり、製品コード・テスト変更、ビルド、テスト実行、GUI確認はしていない。利用者は「URL直接書いてもリンクにならない」と報告した。Windowsの既存MarkdownリンクのCtrl＋クリック不達は別Issue #670であり、この機能追加と混ぜない。

## 読解済みの既存契約

`crates/markdown/src/parser.rs::build_tree` はpulldown-cmarkのoffset event streamからtreeとlink metadataを作る。`LinkType::Inline / Reference / Collapsed / Shortcut` と `is_external_http_url` に該当するdestinationだけが `LinkParse` に入る。`<https://example.com>` のautolinkと裸URLは、今回利用者が求めた直接記述と同じものと仮定しない。現時点でbare URLの自動認識経路は確認できない。

`crates/markdown/src/syntax.rs` の `LinkParse` / `ParseSnapshot.links`、`crates/presentation/src/display.rs` の `LinkTarget` / `VisualLine.links` がsource byte範囲とdestinationを保持する。既存tree nodeとの対応、半開区間、全体offset、reference定義更新、snapshot/cacheの鮮度契約を維持する。新たな候補がtreeのLink nodeを持たない場合の表現は、実装前に現在の利用箇所を調べて決める。架空node IDや既存Markdown nodeの誤用で接続しない。

`crates/ui/src/view.rs` の `row_hit_at_window_point` / `row_hit_at_point` / `link_destination_at` / `apply_row_click` は表示行と実glyphに対応するsource範囲を照合して `cx.open_url` へ渡す。裸URLも同じ入口へ流し、UIでURLを再parseしない。クリック仕様、閲覧pending pressと4論理px閾値は変更しない。

workspaceのGPUIは `gpui-pre = 0.3.6`。active OS backendはpatchされた `vendor/gpui-pre-macos` / `vendor/gpui-pre-windows` で、旧 `vendor/gpui` の実装をactiveと仮定しない。このIssueでOS起動経路を新設しない。

## 採用する認識方式（2026-11-04確定）

Jevにcurrent code・Issue契約・一次仕様・候補を渡し、`local_scanner`、`write_design`、`local_design`を採用した。要求model `jev-latest`、応答model `jev-1.14.0`、各Choice confidenceは0.62 / 0.85 / 0.98。usageはinput 2287 / output 122 tokens。意味判断の記録であり実装・検証成功の証拠ではない。

新しい依存は追加しない。既存pulldown-cmark 0.13.4の`ENABLE_GFM`はblockquote tagsの設定で、裸URL認識を有効化するものではない。既存`url = 2.5.8`は候補の構文とhostの検証にのみ使い、serialize/normalizeした文字列をdestinationへ書き戻さない。

一次資料は [GFM §6.9](https://github.github.com/gfm/#autolinks-extension-) と [linkify 0.11.0公式API](https://docs.rs/linkify/0.11.0/linkify/)。GFMの句読点・括弧の扱いを参考にするが、www/emailも含む完全互換は採用しない。linkifyの候補認識とbyte offsetも検討したが、Hane固有の対象範囲・除外・境界の追加は必要であり、今回は既存依存を使う局所scannerを採用する。以下の具体規則が本Issueの仕様である。

### 正式parseとmetadataの接続

1. `build_tree`の正式なevent/treeから対象を限定する。生の全文に一律regexを掛けない。対象は`NodeKind::Text`の元source範囲。Markdown装飾の意味を先に確定し、装飾nodeを跨ぐURLは作らない。
2. 同じparentの隙間なく連続するText leafは一つのsource runとして扱える。entity/escapeでEvent::Textが分割されても、eventの復号済み文字列ではなく元source sliceを走査する。改行や別kind・別parentは結合しない。
3. CodeBlock（indented/fenced）、InlineCode、HtmlBlock、Html、InlineHtml、全種類のLink（autolinkを含む）、Imageおよびそれらの子孫は除外する。inline HTMLを含む最小のinline block（Paragraph / Heading / TableCell、存在しなければ当該inlineを所有するblock）は全体を保守的に裸URL認識から除外する。同blockのHTML外本文も今回は対象外とし、次の通常blockには影響させない。HTML elementの内外を独自HTML parserで推測しない。
4. 裸URLの`LinkParse.node`は開始byteを含む実在するText nodeとし、`text_range`は半開絶対source範囲、`destination`はその範囲の原文そのもの、`uses_reference=false`。同じText ownerに複数の裸URLを許容する。既存inline/referenceのownerは従来のLink nodeを維持する。架空node ID、Link nodeの新造、TextをLink kindへ偽装する方法は使わない。関連する型コメントの「必ずMarkdown Link node」前提を更新する。
5. 既存リンクと裸URLをsource順・非重複で一つの`links`へ収める。`links_intersecting`の二分探索契約を維持する。block index / list・table projection・incremental/background snapshotにも同じ正式metadataを渡す。
6. presentationは裸URLのmetadata範囲を既存source mapへ投影してLink色と既存`LinkTarget`を生成する。treeのLink kindがない裸URLでも色が付くようにし、通常リンクへの二重styleや重複targetを避ける。cache再利用・offset移動・local parseの対象領域で同じmetadataが失われないようにする。UI製品コードのclick処理、modifier条件、GPUI backendは変更しない。必要ならUIのfocused testのみ追加する。

### 開始・終端の確定規則

- 開始はASCII大小を問わない`http://` / `https://`だけ。直前のsource文字がASCII英数字または`+-.@`なら拒否する。それ以外の文字（日本語、空白、句読点、括弧等）またはrun/文書先頭では許可する。run先頭でも直前source文字が取れる場合は確認し、event分割を境界偽装に使わない。
- Unicode whitespace/control、ASCII `<` `>`、単引用符・二重引用符・backtick・backslash、和文の `。 、 ！ ？ ： ； （ ） 「 」 『 』 【 】 〈 〉 《 》` で終了する。和文句読点はURL途中でも区切りとする。これらをURLへ含めたい場合はpercent-encodingまたは明示Markdownリンクを使う。
- ASCIIの`()` / `[]` / `{}`は種類ごとの対応を守るものを保持する。対応のない閉じ括弧は取り込まず、末尾の未対応開き括弧も除く。IPv6 hostの`[::1]`は保持する。括弧を文脈なくすべて削らない。
- 候補末尾のASCII `. , : ; ! ? * _ ~` を反復除去する。対応しない末尾括弧除去との組合せでも安定するまで処理する。query/fragment内の`? & = # / %`や内部の句読点は保持し、`%29`等はdecodeしない。
- `url::Url::parse`が成功し、schemeがHTTP(S)、hostが存在する候補だけを採用する。`https://`のような入力途中、空host、他schemeは採用しない。canonical URLへ置換せず、大小文字・Unicode・percent表記・query/fragmentを原文のまま保存する。
- Markdown構文を優先する。Textと装飾間を跨いで原文をURLへ再解釈しない。HTMLやリンク記法を変更しない。物理改行を跨がず、表示上のsoft-wrapは同じURLのまま維持する。

### 必須の境界期待値

表の「なし」は裸URL metadataがないこと。既存Markdownリンクは別途従来どおり保つ。全caseでdestinationは表示した期待URLの原文bytesに一致し、source範囲外へ色やhit targetを広げない。

| 元source | 裸URLの期待destination |
|---|---|
| `https://example.com` / `http://example.com` | それぞれ全体 |
| `HTTPS://Example.com/A%2f?q=1&lang=ja#Top` | 大小文字・percent表記も含め全体 |
| `https://example.com/a?q=1&b=two#part.` | `https://example.com/a?q=1&b=two#part` |
| `(https://example.com/a_(b)).` | `https://example.com/a_(b)` |
| `https://example.com/a_(b)))` | `https://example.com/a_(b)` |
| `https://example.com/a(` | `https://example.com/a` |
| `https://example.com/a[1]{two}` | 全体 |
| `http://[::1]:8080/a` | 全体 |
| `https://example.com/a%29` | 全体、decodeしない |
| `https://example.com/a?!` | `https://example.com/a` |
| `参照：https://example.com/資料?q=日本語#節。次` | `https://example.com/資料?q=日本語#節` |
| `「https://example.com/a」、https://example.org/b` | 2個、それぞれ括弧・読点を除く |
| `🙂 https://a.example/ https://b.example/終` | 2個、絶対byte offsetと終端UTF-8境界が正しい |
| `https://example.com/a` + tab/space/CRLF + `next` | 空白前まで。改行後へ連結しない |
| `xhttps://example.com` / `abchttps://example.com` / `me@https://example.com` | なし |
| `https://` / `www.example.com` / `user@example.com` / `file:///a` / `ftp://example.com` / `relative.md` | なし |
| `*https://example.com*` | `https://example.com`、既存Italicは維持 |
| `https://example.com/a*b*c` | `https://example.com/a`、Markdownの`*b*`を跨がない |
| `https://example.com/a_b_c` | 全体（語中underscoreはText） |
| `https://example.com/?a=1&amp;b=2` | 全体の原文（`&amp;`を`&`へ変換しない） |
| `` `https://example.com` `` / fenced・indented code中の同文字列 | なし |
| `[https://label.example](https://dest.example)` / `![https://alt.example](https://img.example)` | なし。前者の既存外部リンクだけを保つ |
| `<https://example.com>` | なし、既存autolinkの挙動は拡張しない |
| `<span>https://example.com</span> https://outside.example`（同paragraph） | 両方なし。次paragraphの通常URLは認識 |

`www`・email・file/他scheme・相対ファイルの新対応、angle autolinkの新対応、Markdown記法への自動書換え、HTML表示機能は対象外。

## 検証・受入への対応

受入条件はIssue本文を正とする。parserで境界と除外・offset、presentationで色とsource↔visual対応、UIで両mode/OS modifier条件と編集state不変をfocusedに検証する。soft-wrap、URL末尾glyph、scroll/zoom後を確認し、適切な回帰テストを追加する。全域再設計や無関係な性能最適化を混ぜない。認識処理の増分・背景解析への影響がある場合は同じ入力の変更前基準を取り、全文の毎回走査をUIへ追加しない。

GUIは両OSで編集（macOS Command / Windows Ctrl）、閲覧（無修飾mouse-up）を確認する。編集通常クリックは選択、閲覧の4論理px未満は開く、以上はドラッグ選択、MouseMoveなしreleaseも既存契約を維持する。開くだけでcaret/selection/bytes/revision/Undo/Redoを変えないことをstateテストと前後観測で確認する。ブラウザの実到達URL・対象head/base・binary・PID/path・隔離state・mode・入力と結果を記録し、起動要求だけを到達成功にしない。

Windows GUIは許可済み既存Windowsチャットから直接操作し、Mac経由VMを使わない。GUI操作前にhane-gui-test skillと共有GUIの利用状況を確認する。失敗時は `docs/gui-test-recovery.md` に従い復旧し、unknownをpassにしない。自分の検証アプリだけを終了確認後に片付ける。

## 並行開発と引継ぎ

#670 / PR #674はWindowsの既存Markdownリンク不達を調査する別PR。parser/presentation側の独立した設計は進められるが、click handlingを両PRで同時変更しない。PR #674が修正するactive backendやhit-testの状態を再取得し、最終Windows GUIに必要な経路をcurrent candidateへ統合して確認する。起動不達を裸URL認識の追加で直ったと主張しない。

#598の相対リンク、#614/#615のcursor、#629/PR #630の挿入機能、#567のview分割も責務が重なる範囲だけ再確認する。#297/RF0全体完了は依存にしない。調査時点でこれらのopen PRはなかったが、着手時にcurrent factsを読み直す。

共有checkout `/Users/hide/dev/Codex/hane` には既存の未commit `crates/ui/src/view.rs` 変更がある。受付は `/tmp/hane-url-link-intake-20261104` の独立cloneで文書だけ作成し、共有変更を上書きしていない。後続は独立した作業場所を使い、共有branchのcheckout/reset/stashをしない。

通常の意味判断には既存Jevを使用。公開factsによる `split` と独立文書準備 `prepare` を採用し、要求model `jev-latest` / 実行model `jev-1.14.0` を確認した。受付はDraft PR作成まで。設計詳細の確定、Claudeへの一度の正規依頼、prepare受理確認、実装以降は別PR担当チャットが行う。

## 実装依頼前後の残条件

この設計確定commit時点でも製品コード・テストの変更なし。新ADRは不要とJevが選択した局所拡張である。依頼直前にexact head / main / 権限 / 同じ対象runを再取得し、default branchの受付契約に従って一度だけClaudeへ依頼する。`prepare`成功を受理証拠とし、実装patch・Sonnet 5.5実使用・finalizer push・verify・両OS CI・レビュー・GUIは別々に確認する。

性能測定では実装前の基準headと変更後headを区別し、同じ環境・fixture・release条件でparser/BlockIndexの処理時間と再parse範囲を比較する。UIでの全文走査追加は禁止。性能測定の記録はPR evidenceへ残し、この文書の設計確定を測定成功と扱わない。
