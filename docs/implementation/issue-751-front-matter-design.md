# Issue #751 Front Matter 対応の実装設計書

## 決定事項の要約

- バイト 0 の `---` 行から、後続の最初の `---` 行までを Front Matter として認識する。閉じ行が見つからなければ通常の Markdown として扱う。
- Markdown パーサに `NodeKind::FrontMatter` を追加し、本文だけを pulldown-cmark に渡す。区切りとメタデータの原文は文書バッファに保持する。
- 編集モードでは通常、日付を先頭にした要約を折り返しなしの 1 行で表示する。キャレット、選択、IME、文書内検索の現在の一致が触れたときに原文を展開する。
- 要約行のクリックと上下移動は、日付の値の先頭など、規則で決まる一つのソース位置に着地する。プレーンテキストモードでは常に原文を表示する。
- 作業フォルダーの新規ノートは `date: "YYYY-MM-DD"` を含む Front Matter と空の `# ` で始め、キャレットを `# ` の直後に置く。
- 自動命名は有効な `date` と本文の最初の空でない ATX H1 から `YYYY-MM-DD_タイトル.md` を作る。日付が欠落・不正なら H1 のみを使う。既存ノートの自動移行は行わない。
- 実装は同一 PR 内で、認識・編集表示、次いで新規ノート・命名の順に進める。設計の基準は `origin/main` の v0.49.0、`6dd7e8e4`。 [Issue #751](https://github.com/hide212131/hane/issues/751) · [基準コミットの Cargo.toml:22](https://github.com/hide212131/hane/blob/6dd7e8e4/Cargo.toml#L22)

## 1. Front Matter の認識規則

開き行と閉じ行は同じ規則で判定する。改行を除いた行が ASCII の `---` で始まり、その後に ASCII スペースまたはタブだけが続く場合に一致する。先頭のインデント、4 個以上のハイフン、行末コメント、全角空白は許容しない。行末は LF、CRLF、単独 CR を扱い、閉じ行は改行なしの EOF でも成立する。各行に元のバイトオフセットを付け、改行を正規化しない。既存の `markdown_lines` の行分割を再利用する。 [crates/markdown/src/markers.rs:239](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/markdown/src/markers.rs#L239)

- 開き行はバイト 0 に限定する。UTF-8 BOM、先行空行、先行空白があれば Front Matter は成立しない。
- 開き行の次の行から探し、最初に一致した独立行を閉じ行とする。キーの有無や引用符の状態は閉じ行の判定に使わない。
- 閉じ行がなければ、開き行を含む文書全体を通常の Markdown として解析する。`---\n---` と `---\n\n---\n` はどちらも空の Front Matter として成立する。
- 一組が成立した後の `---` や、開き行がバイト 0 にない文書の `---` は通常の Markdown として扱う。
- 認識結果は、開き行・中身・閉じ行・全体の半開バイト範囲と、閉じ行末に改行があるかを持つ。全体の範囲に閉じ行自身の改行を含め、直後の空行は含めない。

## 2. パーサとブロックインデックス

`hane-markdown` に文書先頭を判定する `front_matter` モジュールを置く。全文パースと命名処理が同じ認識関数を使用する。`build_tree` の root は従来どおり入力全体を覆う。認識時は最初の子として `NodeKind::FrontMatter` の葉を追加し、`is_block()` の対象にする。pulldown-cmark の `into_offset_iter()` には閉じ行直後からの本文を一度だけ渡し、イベントの範囲には本文開始のバイトオフセットを加算する。Front Matter 内から見出し、罫線、リスト、リンク参照定義、フェンス、装飾マーカーを生成しない。本文中の参照定義は本文全体を解析する既存経路で解決する。 [crates/markdown/src/syntax.rs:13](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/markdown/src/syntax.rs#L13) · [crates/markdown/src/parser.rs:77](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/markdown/src/parser.rs#L77) · [crates/markdown/src/parser.rs:508](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/markdown/src/parser.rs#L508)

正式インデックスでは `tiled_blocks` の既存規則を維持する。先頭ブロックが次の構文ノードまでの空行や参照定義を含む場合があるため、Front Matter の正確な範囲、閉じ行番号、要約用の値とそのソース位置は別途、当該ブロックに付随する情報として保持する。以降の折り畳み・開閉判定・ナビゲーションはこの正確な範囲を使う。タイルに含まれる後続行は通常の本文側の行として扱う。 [crates/markdown/src/block_index.rs:839](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/markdown/src/block_index.rs#L839) · [crates/markdown/src/block_index.rs:900](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/markdown/src/block_index.rs#L900)

増分解析と初回表示では、**部分文字列の先頭と文書の先頭を区別する**。

- 全文パースは先頭認識を行う。増分ウィンドウ、viewport のローカル解析、joined parse には、検証済みの先頭範囲を渡すか、先頭認識を明示的に無効にする。ウィンドウがバイト 0 から始まることだけで、ウィンドウ末尾を EOF とみなさない。
- 先頭行が開き行に一致しない通常の文書は、先頭行の変更がない限り既存の増分経路を使う。確定済みの閉じ行とその改行より後だけの編集では、Front Matter の範囲と解釈を再利用する。閉じ行が改行なしの EOF にある場合、EOF への挿入は閉じ行の再判定対象とする。
- 先頭候補があり、閉じ行に影響し得る編集では、現リビジョンの先頭から閉じ行を再判定する。旧閉じ行が編集後も同じ行として残ると確認できれば、既存の増分経路でブロックと要約を更新する。生成・消滅・別の閉じ行への移動では、旧末尾との一致で再同期せず、既存のバックグラウンド全文パースで索引を入れ替える。
- 同期判定は既存の増分再同期のバイト予算内に収める。予算内に閉じ行も実 EOF も見つからない場合は、未確定の範囲を現リビジョンの原文行として表示・操作し、正式パースで確定してから投影する。初回や再構築中も同様に扱い、旧リビジョンの合成行はキャッシュから除外する。大きな未閉鎖文書の末尾に `---` を追加した場合も、全文パースの完了時に正しく認識する。 [crates/markdown/src/block_index.rs:1103](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/markdown/src/block_index.rs#L1103) · [crates/markdown/src/local_block_index.rs:69](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/markdown/src/local_block_index.rs#L69) · [crates/ui/src/view.rs:2675](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L2675)

## 3. メタデータの解釈範囲

解釈対象は中身にあるトップレベルの単一行 `key: value` とする。最初の `:` で分け、キーは行頭から `[A-Za-z_][A-Za-z0-9_-]*` に一致し、キーと `:` の間には ASCII スペース・タブを許容する。値の外側のスペース・タブを取り除く。値全体が対応する単一引用符または二重引用符で囲まれていれば、一組だけ外して中身を使う。外した内側の空白は値に含まれる。値の引用符のエスケープや YAML の型変換は行わない。

空行と、先頭のスペース・タブを除くと `#` で始まるコメント行は要約に数えない。インデントされた項目、空値、不整合な引用符、引用符内のエスケープを必要とする値、インラインコメント、配列・マップ・複数行スカラーなどは解釈せず、原文のまま保持する。具体的には、引用なしの値で `[`、`{`、`|`、`>`、`&`、`*`、`!`、`#`、または `-` に空白が続く形から始まるもの、空白に続く `#` や `:` の後に空白を含むものを要約対象外とする。`url: https://example.com` のような値は表示できる。解釈対象外の行があってもブロックの認識や編集には影響させない。

`date` は小文字のキー名と完全一致し、宣言が 1 件だけで、その値が ASCII の `YYYY-MM-DD` かつ実在するグレゴリオ暦の日付の場合に有効とする。年は `0001`〜`9999`。`date: 2026-11-21`、`date : "2026-11-21"`、`date: '2026-11-21'` は有効。`2026-02-29`、`2026-1-2`、`" 2026-11-21 "`、空値、重複した `date` は無効とする。キーが字句上 `date` に一致すれば、値を解釈できない行も重複判定に数える。表示と命名は同じ検証済みの値を使う。検証結果には、引用符を除いた値のソース範囲を持たせる。

## 4. 折り畳み表示と SourceMap

| 入力 | 要約行 |
| --- | --- |
| 有効な日付だけ | `2026-11-21` |
| 有効な日付と他項目 | `2026-11-21 · author: 佐藤 · project: Hane` |
| 日付なし、解釈できる他項目あり | `メタデータ · author: 佐藤` |
| 日付の宣言あり、値が不正または重複 | `メタデータ · 日付無効` に続けて、表示できる他項目 |
| 空行・コメントだけ | `メタデータなし` |
| 解釈対象外の行だけ | `メタデータ` |

他項目はソース順に最大 2 件表示する。残りの解釈可能な他項目は ` · +N` とし、同じキーは最初に解釈できた値だけを数える。要約中の空白は 1 スペースにまとめ、キーと値をそれぞれ最大 24 書記素に収め、超過時は末尾を `…` にする。レイアウトでは実測した列幅に合わせて行末をさらに省略し、`…` も収まらなければ表示可能な位置でクリップする。日付がある場合は日付から描き始める。テーマに応じた淡い背景と判読できる前景色を使用し、行高は本文の標準 1 行分とする。狭幅や拡大時も折り返さない。 [crates/presentation/src/layout.rs:982](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/presentation/src/layout.rs#L982)

要約は開き行の `line_id` に置く。`SourceMap` には要約全体を覆う `Visibility::Synthesized` セグメントと、長さ 0 のソースアンカーを持たせる。アンカーは **有効な日付値の先頭 → 最初に要約に載る他項目の値の先頭 → 開き行の先頭** の順に選ぶ。行の字形、区切り記号、省略記号、右側の行内余白をクリックしても、同じアンカーに着地する。Shift を伴うクリックでは既存の選択アンカーを保持し、アクティブ端だけを移す。要約のバイト位置から原文位置を補間しない。 [crates/presentation/src/source_map.rs:47](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/presentation/src/source_map.rs#L47) · [crates/ui/src/view.rs:6568](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L6568)

表の区切り行と同じ高さ計算を、**開き行の次から閉じ行まで**の省略に利用する。開き行の要約には高さと入力対象がある。省略行には独立したヒット領域や上下移動の停止点を作らない。viewport 内外の省略行数は範囲の交差から求め、各行の読み込みやシェーピングを省く。`VisualBlock::covers` は省略済みのソース範囲も表示決定済みとして扱い、`visible_line_prefix` では開き行の直後から閉じ行までの行を差し引く。後続の空行は通常どおり高さを持ち、着地できる。 [crates/presentation/src/projection/block.rs:534](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/presentation/src/projection/block.rs#L534) · [crates/presentation/src/display.rs:667](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/presentation/src/display.rs#L667) · [crates/ui/src/view.rs:5004](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L5004)

## 5. 展開条件、キャレット、再描画

編集モードでは、次のいずれかを満たす間、両方の区切り行を含む Front Matter 全体を原文表示する。

- キャレットが Front Matter の半開範囲内にある。例外として、閉じ行に改行がなく文書がそこで終わる場合は、EOF のキャレットも閉じ行に属する。
- 長さのある選択範囲または IME のマーク範囲が Front Matter と交差する。長さ 0 の IME 範囲はキャレットと同じ所有規則を使う。
- 文書内検索バーの現在の一致が Front Matter と交差する。検索欄にフォーカスがあっても該当箇所を原文で表示する。他の一致は検索結果に含め、開閉条件には加えない。 [crates/ui/src/line.rs:53](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/line.rs#L53) · [crates/ui/src/view/document_find.rs:393](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/document_find.rs#L393)

折り畳み中に下から `↑` で入る場合、直前に表示されている行が要約行になった段階で、第 4 節のアンカーに着地する。閉じ行直後に空行があれば、本文からまずその空行に移動する。バイト 0 にあるため、現行仕様で要約より前に移動元となる行は存在しない。上下の隣接行解決には同じアンカー規則を適用し、文書先頭で上へ移動したときは既存どおり留まる。`Shift+↑` は選択端を同じアンカーに置く。`Home` や左移動、検索結果へのジャンプなど、すでに具体的なソース位置を持つ操作では、着地した位置をそのまま使う。 [crates/ui/src/view.rs:4174](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L4174) · [crates/ui/src/view.rs:4646](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L4646)

状態変更に伴うキャレット再配置は行わない。クリックや上下移動で先にソース位置を決め、その位置で次のレイアウトを展開する。展開中の行は `present_raw_source` 相当の行内等長マップを使い、罫線・見出し・リンク・リストとしての装飾や入力補助を適用しない。原文は通常どおり折り返し、viewport に必要な行だけを投影する。すべての展開条件を外れた時点で要約に戻す。

`BlockWindow.block_disclosure` の既存経路に載せ、開閉判定には Front Matter の正確な範囲を使う。`disclosures_are_current` では、合成行の `lines.len()` から物理行の表示範囲を逆算する前に、キャッシュされた開閉状態と現在の状態を照合する。開閉、範囲、要約の変更時は presentation と layout を作り直す。ブロック内の移動で開閉が同じなら、行カバレッジが保たれる限りキャッシュを再利用する。 [crates/presentation/src/projection/block.rs:242](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/presentation/src/projection/block.rs#L242) · [crates/ui/src/view.rs:5403](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L5403)

初期の高さ推定、増分更新、スクロールの行換算に同じ開閉判定を使う。既存の高さ処理はキャレット・選択・IME を主に参照するため、Front Matter では検索の現在の一致も渡す。開閉時は先頭ブロックの高さを同期更新し、既存のスクロールアンカーとレイアウト後のキャレット再表示確認を使う。展開した長行の実測高を畳んだ状態へ持ち越さない。 [crates/presentation/src/projection/block.rs:53](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/presentation/src/projection/block.rs#L53) · [crates/ui/src/view/height_sync.rs:37](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/height_sync.rs#L37) · [crates/ui/src/view/height_sync.rs:442](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/height_sync.rs#L442)

## 6. 編集操作と文書状態

- **区切りの編集**。次の文書リビジョンで第 1 節を再適用する。閉じ行を消して他に該当行がなければ通常の Markdown に戻る。後方に別の `---` 行があれば、それが新しい閉じ行になる。キャレットと選択は既存の編集処理が返したソース位置を使う。Undo で区切りが戻れば認識も戻る。
- **テキスト操作**。文字入力、改行、削除、貼り付け、IME は通常のエディタトランザクションで処理する。Front Matter 内の `- item` などはリスト補完の対象外とし、増分索引が未確定の間も先頭候補の原文表示に対して構文依存の編集補助を使わない。
- **検索・置換**。検索対象は区切りを含む文書バッファ。全置換は要約に載らない行も対象になり、既存どおり一つの Undo 単位とする。置換による認識の変更は通常の編集と同じ経路を通す。
- **全選択・コピー**。全選択の範囲は文書全体。選択が Front Matter と交差すれば原文が展開される。クリップボードへは選択した原文を渡し、要約行や `…` を混入させない。
- **保存・履歴**。開閉は表示状態のみで表現する。キャレット移動・クリック・検索による開閉でバッファ、改訂番号、保存済みリビジョン、dirty、Undo/Redo の内容を変えない。開閉処理から保存・命名のジョブを発行しない。実編集とその Undo/Redo は既存の保存・命名経路を通す。 [crates/editor/src/lib.rs:332](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/editor/src/lib.rs#L332) · [crates/ui/src/actions.rs:751](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/actions.rs#L751) · [crates/ui/src/view/document_find/replace.rs:564](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/document_find/replace.rs#L564) · [crates/ui/src/view.rs:2675](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L2675)

## 7. プレーンテキストモードと閲覧モード

`DisclosurePolicy::Plain` と `PlainHighlighted` は常に `present_block_plain` を使い、区切りを含む全ソース行を表示する。カーソルで原文をそのまま検査・編集するというプレーンテキストモードの契約を維持する。ハイライト有効時も Front Matter に Markdown の見出し・罫線等の装飾を適用しない。モード切替前後で選択・キャレットのソース位置を保持する。Markdown の閲覧モードでは要約 1 行を表示し、編集モードへ戻った時点で現在の選択・キャレットに従って開閉を決める。 [Issue #737](https://github.com/hide212131/hane/issues/737) · [crates/presentation/src/projection/block.rs:147](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/presentation/src/projection/block.rs#L147) · [crates/ui/src/view/reading_mode.rs:101](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/reading_mode.rs#L101)

## 8. 新規ノートの開始形

`new_work_folder_note_heading()` を、日付を受け取って初期テキストを返す `new_work_folder_note_text(today: CalendarDate)` に置き換える。`new_work_folder_note_in` は `local_today()` を 1 回呼び、ルート・選択中のサブフォルダー・コンテキストメニューの各入口で同じ初期化を使う。2026 年 11 月 21 日の正確な入力は次の Rust 文字列リテラルで表せる。

```rust
"---\ndate: \"2026-11-21\"\n---\n\n# "
```

改行は LF。閉じ行の後に空行が 1 行あり、`#` の後に ASCII スペースが 1 個、末尾改行はない。キャレットは `SourceOffset(text.len())`、つまりバイト 30 に置く。既存の対象ディレクトリ検証と復元用下書き登録を使う。有効な H1 が書かれるまでは無題の下書きとして扱い、日付だけで自動作成しない。明示的な保存は既存の経路を使う。 [crates/ui/src/view.rs:2751](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L2751) · [crates/ui/src/view.rs:2808](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L2808)

## 9. タイトル同期と改名

`hane-session` の既存の `hane-markdown` 開発時依存を通常依存へ移し、先頭認識と日付解釈を共有する。`naming.rs` で認識結果を一度求め、`extract_h1_title` は本文部分だけを既存の H1 抽出ロジックに渡す。Front Matter 内の `# コメント` やフェンスに似た行は H1 抽出から除外される。本文では最初の空でない ATX H1 を引き続き使い、フェンス内を飛ばす。Setext 見出しは命名の対象外とする。認識されない先頭や Front Matter のない文書は全文を既存の抽出器に渡す。 [crates/session/Cargo.toml:8](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/Cargo.toml#L8) · [crates/session/src/naming.rs:11](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/naming.rs#L11)

| 文書の状態 | 自動命名の候補 |
| --- | --- |
| 有効な `date` と有効な本文 H1 | `YYYY-MM-DD_` とサニタイズ済み H1 を連結 |
| `date` が欠落・不正・重複し、本文 H1 は有効 | サニタイズ済み H1 のみ |
| Front Matter がない | 現行の H1 由来名。`# 2026-11-10_week-note` は `2026-11-10_week-note.md` |
| H1 が空、存在しない、またはサニタイズで有効な文字列が残らない | 自動作成・改名を行わない。保存済みの名前は保持 |

先に H1 を `sanitize_title` で検証し、日付を使う場合も最終の拡張子なし候補は既存の 100 文字上限に収める。日付付きの候補では接頭辞が 11 文字を使う。H1 をユーザーが日付入りで書いた場合も H1 の内容を通常どおり使う。メタデータの日付を優先して H1 の文字列を書き換える処理は設けない。 [crates/session/src/naming.rs:9](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/naming.rs#L9) · [crates/session/src/naming.rs:86](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/naming.rs#L86)

`run_title_sync` は現リビジョンの全文から候補を再計算する。既存の 750 ms デバウンス、作業フォルダーと文書インスタンスの検証、保存・改名の直列化、上書き防止を使う。**すでに自動命名を追跡しているセッションでは、H1 と同様に date の確定編集にも追従する。** 不正な値がデバウンス後も残れば H1 のみへ改名し、有効な値に戻れば日付付きへ戻る。手動改名したファイル、通常のファイルを開いたセッション、開き直して追跡状態を持たないファイルは現在の自動命名の対象範囲に従う。既存ノートの本文走査による一括移行は設けない。 [crates/ui/src/view/session_save.rs:202](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/session_save.rs#L202) · [crates/ui/src/view/session_save.rs:281](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/session_save.rs#L281) · [crates/session/src/session.rs:500](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/session.rs#L500)

同名衝突で `… 2.md` になった Front Matter ノートも追従させる。現行の比較は `auto_title` と実ファイルの stem が一致する前提なので、Front Matter 由来の命名に成功したセッションでは **導出した候補 stem と、実際に確定した stem の両方**を記録する。現在の stem が実績と違えば追跡を停止し、導出候補が前回と同じなら現在の衝突回避名を維持する。追跡状態は成功した書き込み・改名の確定時だけ更新し、従来ノートには既存の判定を適用する。 [crates/session/src/naming.rs:124](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/naming.rs#L124) · [crates/session/src/naming.rs:181](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/naming.rs#L181) · [crates/ui/src/view/session_save.rs:646](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/session_save.rs#L646)

名前の空き確認中に date や H1 が変わり得る。自動作成では、確認完了後、保存を開始する直前に現リビジョンと最新候補を照合し、古ければ現状態から再判定する。実行中の改名が完了した場合は、同一インスタンスで成功した移動先を既存の経路で反映し、保留された最新編集があれば再判定する。失敗時に同じ候補で無条件に再試行せず、既存の保存待ちとセッション終了処理を解放する。 [crates/ui/src/view/session_save.rs:375](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/session_save.rs#L375) · [crates/ui/src/view/session_save.rs:559](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/session_save.rs#L559) · [crates/session/src/service.rs:654](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/service.rs#L654)

## 10. サイドバーの日付バッジと並び替え

サイドバーの入力は引き続きファイル名と更新時刻。日付バッジは実際にはファイル名の途中の日付にも対応しており、`YYYY-MM-DD_タイトル.md` の先頭日付を既存の規則で表示できる。date が欠落・不正になり H1 のみで改名した場合は、そのファイル名から既存規則で日付を探す。H1 に別の日付がなければバッジは消える。自動命名の対象外のノートは、date を変えてもバッジの入力となるファイル名が変わらない。 [crates/ui/src/view/sidebar.rs:2393](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/sidebar.rs#L2393) · [crates/session/src/date_badge.rs:256](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/date_badge.rs#L256)

名前順は改名後のファイル名で再配置される。更新日時順は既存の mtime と名前によるタイブレークを使う。改名そのものでは葉の既知 mtime を引き継ぎ、date の編集内容が保存されたときは既存どおり保存された mtime が反映される。改名後のサイドバー、タブ、ピン、最近使ったファイルは既存の成功処理を通じて新しいパスへ追従させる。 [crates/session/src/workfolder.rs:433](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/workfolder.rs#L433) · [crates/session/src/workfolder.rs:669](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/session/src/workfolder.rs#L669) · [crates/ui/src/view/session_save.rs:573](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/session_save.rs#L573)

## 11. 実装フェーズと検証

同一 PR 内で 2 フェーズに分ける。フェーズ 1 の受入は既存ファイルでの認識、原文編集、表示とソース位置の整合まで。フェーズ 2 の受入は新規ノートの開始形、自動命名、サイドバーの追従までとする。Issue #737 のプレーン表示と既存の索引・disclosure・保存経路を利用するため、基準コミット上で先行リファクタリングは設計上の依存にしない。着手時はこれらの契約に重なる変更を current main と関連 PR で確認する。以下のテスト名は実装時に追加するケースを示す。 [docs/refactor-execution-plan.md:52](https://github.com/hide212131/hane/blob/6dd7e8e4/docs/refactor-execution-plan.md#L52)

### フェーズ 1 — 認識、表示、編集

| 層・テスト名 | 入力と期待結果 |
| --- | --- |
| Markdown `front_matter_recognizes_only_completed_document_prefix` | LF、CRLF、CR、閉じ行 EOF、末尾スペース・タブ、空のブロックを認識する。BOM、先行空行、先頭インデント、欠落した閉じ行、途中の `---` は従来のパース結果と一致する。 |
| Markdown `front_matter_keeps_body_event_offsets_and_isolates_metadata` | ヘッダーに `# コメント`、参照定義、未閉鎖フェンスに見える行を置き、本文に見出し、リンク、罫線を置く。root の第 1 子は子を持たない Front Matter。本文ノードの絶対バイト範囲が原文と一致し、ヘッダー内の参照定義は本文で解決されない。 |
| Markdown `front_matter_metadata_uses_one_valid_date` | 引用符あり・なしと `2028-02-29` を受理し、`2026-02-29`、短い月日、重複、コメント付きの値を拒否する。文字列に日本語を含め、返った値の範囲が UTF-8 境界にあることを確認する。 |
| 索引 `front_matter_index_reclassifies_after_delimiter_edit` | 開き行・閉じ行の削除、後方の代替区切り、途中への早い閉じ行の追加、Undo、Redo を試す。正式解析後の種類・範囲・行数が毎回 `BlockIndex::build` と一致する。ローカル窓の終端やバイト 0 以外の窓先頭が誤認を起こさないこと、予算外からの正式結果が古い投影に負けないことも確認する。 |
| Presentation `front_matter_summary_has_one_row_and_stable_source_anchor` | 日付の行順を後方に置き、長い日本語・絵文字を含む他項目を 3 件にする。日付先頭、2 件と `+1`、狭幅でも 1 行、両方向のバイアスで同一アンカー、後続空行の高さ、空と不正時の表示を検証する。 |
| Presentation `front_matter_disclosure_respects_exact_boundaries` | 閉じ行の改行前・直後、EOF、選択の交差、長さ 0 の IME、現在の検索一致を表形式で検証する。表示・カバレッジ・高さ推定の状態が一致し、展開行のマップは原文の位置に戻る。 |
| GPUI `front_matter_click_and_up_expand_at_the_same_source_offset` | 実 `EditorView` を描画し、要約の字形と右余白への座標イベント、および本文からの実キー `↑`・`Shift+↑` を送る。着地点、選択端、キャレットの描画位置、`↓` で区間を出た後の高さを確認する。開閉のみで原文・リビジョン・dirty・Undo/Redo が変わらない。 |
| GPUI `front_matter_find_replace_copy_and_plain_mode_share_source` | 要約に出ない行を検索・全置換して 1 回の Undo で復元し、全選択のコピーを原文とバイト比較する。表示設定は `StateStores::memory()` で隔離し、プレーン表示・ハイライト有無の切替で行数とソース位置を確認する。 |

実機 GUI `front_matter_native_editing` では、通常幅と狭幅で日付先頭の 1 行表示、クリックと `↑` の着地、`↓` での復帰、マウス選択とスクロールを確認する。日本語 IME で候補表示・確定・取消を行い、変換範囲内で展開が維持され、別の行を誤編集しないことを確認する。CRLF の fixture を開閉だけして、保存されたバイトが変わらないことも確認する。 [crates/ui/src/view.rs:25350](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view.rs#L25350) · [crates/ui/src/view/plain_text_mode.rs:477](https://github.com/hide212131/hane/blob/6dd7e8e4/crates/ui/src/view/plain_text_mode.rs#L477)

### フェーズ 2 — 新規ノートと命名

| 層・テスト名 | 入力と期待結果 |
| --- | --- |
| Session `front_matter_title_sync_uses_body_h1_and_date` | ヘッダーに `# 誤タイトル`、本文フェンス内に `# サンプル`、その後に `# 週報` を置く。`2026-11-21_週報` を得る。欠落・不正・重複時は `週報`、空 H1 と `/` だけの H1 では候補なし。旧式 H1、長い日本語、記号、100 文字上限も確認する。 |
| GPUI `new_work_folder_note_starts_with_front_matter` | 固定日付で初期文字列をバイト比較する。公開入口からサブフォルダーに新規ノートを開き、`# ` の直後で入力可能、要約は 1 行、H1 が空の間は `.md` が未作成、入力後に選択したフォルダーへ原文どおり保存されることを確認する。 |
| GPUI `front_matter_date_rename_tracks_collisions_and_latest_edit` | 既存の同名ファイルを置き、`2026-11-21_週報 2.md` を作る。date を修正すると新しい日付の名前へ追従し、元のファイルを上書きしない。空き確認中・改名中に次の編集を入れ、最後の有効な候補と保存先が一致すること、手動改名後は追従を停止することを確認する。 |
| GPUI `front_matter_rename_refreshes_sidebar_badge_and_order` | 日付が前後する兄弟ノートを用意する。date の変更後にバッジと名前順が更新され、更新日時順では保存時刻の既存規則に従う。date を不正にして `週報.md` へ移るとバッジが消える。別途、旧式ノートの本文と命名を回帰確認する。 |

実機 GUI `front_matter_native_new_note_and_rename` では、作業フォルダーとサブフォルダーの新規ノート入力、最初の保存、date の編集と再保存、バッジと一覧の再配置、開き直したファイルの原文を確認する。GUI 確認は対象 head の Debug ビルドで行い、SHA・バイナリ・結果を記録する。Windows の確認は接続済み Windows 環境で直接行う。GPUI のイベントテストにはネイティブ IME の結果を代用させない。 [docs/agent-desktop-gui-testing.md](https://github.com/hide212131/hane/blob/6dd7e8e4/docs/agent-desktop-gui-testing.md) · [.agents/skills/hane-gui-test/SKILL.md](https://github.com/hide212131/hane/blob/6dd7e8e4/.agents/skills/hane-gui-test/SKILL.md)

両フェーズで対象 crate のテストを先に実行し、PR の最終候補で `cargo test --workspace --all-targets --no-fail-fast` と `cargo fmt --all -- --check` を個別に実行する。通常 Markdown、大きな Front Matter、大きな未閉鎖の先頭候補について、基準コミットと同じ文書・操作で初回表示と編集後の同期作業量を比較する。通常文書の本文編集で先頭ブロックや文書全体の再解析が増えないことを確認する。

## 12. リスクと不採用案

| リスク・方式 | 判断 |
| --- | --- |
| 既存文書の先頭にある二つの `---` を取り込む | 同じバイト列を罫線の組と Front Matter の両方に意図できる。先頭限定と厳密な行照合で対象を限定する。認識された文書はプレーン表示で原文を確認できる。 |
| 合成行に対するクリック位置を文字単位で原文へ逆算する | 日付の並べ替え、区切りの追加、値の省略で原文順との対応が崩れる。固定ソースアンカーを使い、文字単位の編集は原文展開後に行う。 |
| 高さ 0 への完全な折り畳み | 日付が読めず、キーボードやクリックの入口も失われる。要約に 1 行を割り当てる。 |
| フル YAML または厳格な YAML 検証によるブロック認識 | 入力途中の不正で突然ブロック種別が変わり、日付の表示と編集位置が揺れる。区切りで範囲を決め、解釈できる値だけを要約と命名に使う。 |
| 別ペインや専用フォームでの編集 | 二つの入力面のフォーカス、IME、Undo、保存同期が必要になる。既存のエディタ上で原文を編集する。 |
| viewport の表示範囲から閉じ行を推測する | 長いブロックや再構築待ちで表示と編集対象が食い違う。閉じ行の証拠が得られるまで原文を表示し、リビジョンが一致する索引を採用する。 |
| 無効な日付への変更が意図せず名前へ反映される | 有効性を共通判定し、既存の 750 ms デバウンスと追跡対象の範囲を適用する。無効な値が残った場合の H1 のみへの改名をテストで固定する。 |

## 受入条件チェックリスト

- [ ] 文書先頭で閉じた Front Matter が独立ノードになり、内部の行が本文構文や H1 タイトルに混入しない。
- [ ] 有効な日付が要約の先頭にあり、狭幅や拡大時も要約は 1 行に収まる。後続の空行は保持される。
- [ ] クリックと `↑` は定義済みのソース位置に着地する。選択・IME・現在の検索一致で適切に展開し、条件が解消すると 1 行に戻る。
- [ ] 開閉で原文、改訂番号、保存状態、Undo/Redo が変わらない。区切り編集、検索置換、全選択、コピー、Undo/Redo はソース上で整合する。
- [ ] プレーンテキストモードでは区切りを含む全行を表示する。
- [ ] 作業フォルダーの新規ノートは指定の初期バイト列とキャレット位置を持ち、H1 入力後に `日付_タイトル.md` を生成する。
- [ ] 追跡中の date 編集、無効値へのフォールバック、名前衝突、保存との競合で、自動命名と実ファイルが整合する。
- [ ] 既存の `# 日付_タイトル` ノート、文書途中の `---`、サイドバーの日付バッジと並び替えがそれぞれ既存契約に従う。
- [ ] 対象 head でパーサ、presentation、GPUI 統合、ワークスペース全ターゲット、および実機 GUI の結果を確認する。

## ユーザー判断が必要な未確定事項

なし。BOM 付きファイルの扱い、重複日付の扱い、無効な日付への改名時のフォールバックは、本設計の規則で実装に進められる。