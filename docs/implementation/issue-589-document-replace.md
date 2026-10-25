# Issue #589 文書内置換の実装担当向け設計

## 1. 位置付けと開始条件

要求・対象外・完了条件の正本は [Issue #589](https://github.com/hide212131/hane/issues/589)。本書は実装に必要な状態、処理順、保護契約、検証の対応を定める。設計レビュー用の文書であり、製品実装・検証の完了を示さない。実装担当は Claude Code とし、実装開始・レビュー・GUI・受入の手順は default branch の Commander Policy を参照する。

調査基準は 2026-10-26 の main `593726cec4d651fc8ed0fbbfa2e2b878599199bd`。調査時、#589 の実装PRはなく、open PR #620 は設定画面の機械的分離（#567 P4）であった。#620 は設定画面責務を対象とし、本件の検索状態・範囲編集・履歴契約を変更する計画ではないため、その完了を開始条件にしない。実装開始時には最新 main、関連PR、同じ対象のrunを読み直す。

#413 / #415 の検索経路を拡張する。#414 / #417 のフォルダ検索は対象外。入力先 #304 / #305、保存 #309、閲覧モード #547 の受入済み経路を使い、旧経路への追加や全域整理 #297 の完了待ちはしない。同じ責務を変更する新しいPRがあれば、その範囲だけ調整する。

## 2. 現在の接続点

以下のパスは repository root からの相対パス。既存シンボルと追加候補を区別する。追加APIの名前・内部表現は実装担当が決めてよいが、本書の不変条件を維持する。

| 既存パス・シンボル | 現状と今回の変更責務 |
| --- | --- |
| `crates/editor/src/find.rs` の `FindQuery`, `FindOptions`, `scan`, `FindNavigation` | UTF-8 source位置による単一行リテラル検索。大小文字比較、窓単位走査、取消、100,000件上限を再利用し、範囲指定走査を追加する。全文検索APIの既存挙動を保つ。 |
| `crates/ui/src/view/document_find.rs` の `DocumentFindState`, `DocumentFindTarget` | 入力、結果、session/generation/revision、query generation、debounce、実行中job上限を所有。ここに置換欄、対象範囲、準備状態、実行後件数を加える。 |
| 同ファイルの `start_document_find_scan`, `finish_document_find_scan`, `resync_document_find_after_edit`, `resync_document_find_for_active_document` | 非同期結果の採否、本文編集・文書切替による無効化を拡張する。描画やselectionだけで走査し直さない。 |
| `crates/editor/src/lib.rs` の `Editor::replace_range_recorded`、`crates/editor/src/history.rs` | 現状は1範囲・1履歴。1件置換に利用できる。全置換には1回のUndo/Redoと失敗時の原子性を満たす適用経路が必要。 |
| `crates/document/src/lib.rs` の `RopeBuffer`, `EditSummary`, `RevisionDelta` | byte境界検証、source・行index・revision・deltaの整合を維持する。ropeだけを差し替えて既存delta契約を飛ばさない。 |
| `crates/ui/src/actions.rs`、`view/input_routing.rs`、`view.rs` の keystroke interceptors | 検索欄の `gpui_component::input::InputState` は自身の入力を処理する。本文の `InputTarget` と同じ短文モデルに移し替えず、置換欄も同じwidget経路を使い、本文actionへの漏れを防ぐ。 |
| `crates/ui/src/view.rs` の `after_input`, `body_editable` | 成功した編集だけ通常の dirty/parse/disclosure/保存/下書き/H1同期へ通知する。閲覧モードで変更しない。 |

新しい製品責務は必要なら `editor/src/replace.rs`、`ui/src/view/document_replace.rs` に分ける。検索エンジン、保存機構、入力先判定の第二の実装は作らない。既存の ADR-0013（Undo/Redo）、source位置と表示の対応契約を維持する。

## 3. 操作仕様

Ctrl/Cmd+F で開く既存バーに「置換欄を開く」ボタンを追加する。初期版に新しいグローバルショートカットは追加しない。折り畳み中の検索操作は維持する。

展開時は検索語、置換後文字列、大小文字、前後移動に加え、対象「文書全体 / 選択範囲内」、「本文の選択を取り込む」、「置換」、「すべて置換」を識別できる表示にする。バー高さ・本文viewport・スクロール範囲を `document_find_reserved_height` と一致させ、小さいウインドウでも実行ボタンを操作できるようにする。

検索語は既存 `FindQuery::parse` の仕様（空は実行不可、CR/LF不可、4,096 bytes上限）を使う。置換後文字列は空を許し、CR/LFを拒否し、同じ4,096 bytes上限とする。貼り付け経路で改行を黙って削除・連結しない。widgetの見かけが単一行でも、元の貼り付けが不正なら案内して実行不可にする。IME確定前の一時的な文字列を実行用の値にしない。

| 操作 | 決定事項 |
| --- | --- |
| 検索欄の Enter / Shift+Enter | 既存どおり次 / 前へ移動。本文変更はしない。 |
| 置換欄の Enter | IME確定を除き「置換」相当の1件操作。Shift+Enterでは置換しない。全置換はボタンの明示操作だけで実行する。 |
| Tab / Shift+Tab | 検索欄・置換欄・対象選択・ボタン間を移動できる。フォーカス表示を付ける。 |
| Escape | 各入力欄のIME取消を優先。別のEscapeで既存バー終了・本文復帰。準備中ならまず取消して結果適用を禁止する。 |
| 本文クリック | バーを開いたまま本文へ戻る。新しく本文を選択してから「本文の選択を取り込む」を押せる。 |
| バーを閉じる | 準備job・取り込んだ範囲を破棄。再度開く際は新しい本文状態を基準にする。 |

IME中のEnter/Escapeをwidgetが処理した後では判別できない既存事情がある。`note_document_find_enter_keystroke` / `note_document_find_escape_keystroke` とcomposition判定を、現在フォーカス中の検索・置換の両欄へ拡張する。ボタンクリックを含め、本文または入力欄のcomposition中は実行しない。欄からボタンへblurした際も未確定入力を誤実行しない。

`document_find_input_is_focused` 相当の本文キー遮断条件は両欄を含める。Undo/Redo、貼付け、削除、選択、移動の本文actionへ漏れないことを実キーで検証する。欄のUndoはwidget自身の履歴を扱い、本文履歴は変更しない。検索語・置換後文字列の入力だけでは本文を変更しない。

## 4. 対象範囲と結果identity

UIに次の意味を持つ状態を置く（型名は例）。

| 状態 | 内容・寿命 |
| --- | --- |
| `ReplaceScope::Document` | 現在文書の全source。初期値。 |
| `ReplaceScope::Selection` | session、document generation、revision、正規化した半開byte範囲 `[start,end)`。本文selectionとは別に保持する。 |
| 選択の未取得・無効 | 「本文で範囲を選択して取り込んでください」を表示。選択範囲内モードを保ち、全文へフォールバックしない。 |
| 準備中のrequest | 対象文書identity、query/optionsの世代、scopeの世代、置換文字列の世代、実行種別、1件時のcurrent range、取消token。 |
| 実行結果通知 | 操作対象と実変更件数。検索結果の現在件数とは分離し、文書切替や新しい操作で古い成功を誤表示しない。 |

「選択範囲内」へ切り替える時、現在の非空の本文selectionを取り込む。本文を選び直した後は明示的な取り込みで更新する。ボタンのfocusによってselectionを失わないよう、本文に保持されているselectionを使う。逆方向selectionも範囲は正規化し、編集前の方向は履歴に保持する。複数行の本文選択も許す（検索語・置換文字列の単一行制限とは別）。

範囲内検索は範囲を直接走査し、両端に完全に含まれる一致だけ返す。全文結果を取得してからfilterすると、範囲外の一致による上限到達や非重複の走査開始位置が範囲内結果を欠落させるため採用しない。範囲の始端から照合し、末端を越えない。上限・取消はその対象範囲内で判定し、返す位置は文書全体のsource byte座標とする。

前後移動・結果の強調・原文開示は取り込んだ範囲を変更しない。現在の本文selectionや「現在の一致」をscopeの正本にしない。対象範囲の表示は「選択範囲内」とsourceの開始・終了位置（例: 行・列）を示し、現在の一致とは異なる範囲表示で確認できるようにする。行・列の算出を毎フレームの全文走査にしない。

この機能が成功させた置換だけは、範囲の始端を維持し、末端を実際の挿入bytes−削除bytesの合計で更新して新revisionへ結び直す。すべて削除して範囲長0になった場合も、全文モードへは変更しない。外部の本文編集、Undo/Redo、再読込、文書差替えでは対応を推測せずscopeを無効化する。文書切替では古いmatches/current/scope/準備結果を破棄し、新文書の「選択範囲内」は未取得として明示的に取り込むまで実行不可にする。検索語・置換文字列の保持はよいが、古い置換を新文書へ自動適用しない。

検索結果に既存の `(SessionId, generation, Revision, query_generation)` に加えてscope identityを持たせる。置換の準備結果は置換文字列の世代とcurrent rangeも持つ。検索結果受信時と、本文へ適用する直前の両方で照合する。scopeやqueryを変更して元の値へ戻した場合も、古い世代のjobは採用しない。本文selectionだけの変化ではrevisionを変えず、取り込んだscopeも無効にしない。

## 5. 1件置換

1. Readyでcurrent一致があり、対象identityが一致し、入力が有効、`body_editable()`、本文・両欄のIMEなし、準備jobなしを確認する。UIのdisabled表示だけに依存せず実行handlerでも確認する。
2. current一致がscopeに完全包含され、UTF-8境界と元sourceの照合条件を満たすことを検証する。検索語のbyte長で範囲を再計算しない（大小文字無視では一致元のbyte長が違う場合がある）。
3. 元sourceと置換後が同じbytesなら履歴・revision・dirtyを変えず変更0件とするが、次へ進む。異なる場合は1回の記録付き置換として適用する。
4. 成功後の本文selectionは挿入末端のcaretとし、前後selectionを履歴に記録する。scopeと結果の世代を先に更新し、自分の成功を通常編集によるscope失効と誤認しないようにして `after_input` へ一度通知する。
5. 変更後の文書を再検索し、挿入区間の末端以降の一致から次を選ぶ。削除時は同じbyte位置へ詰まった未処理の次の一致を選べるようにする。末端以降になければ先頭側へ折り返すが、この操作で挿入した区間に重なる一致を直後のcurrentにしない。候補がそれだけならcurrentなしとして、再度の明示的な前後移動で選べるようにする。

例: `a a` の先頭を `aa` に置換すると `aa a` になり、次のcurrentは末尾の `a`。`aa` を `a→空` で1件削除すると、残りの `a` がcurrentになる。同一文字列への置換で同じcurrentに留まり続けない。照合・表示はMarkdown原文の位置を使い、記号・リンク先・折り畳まれたcode fenceでも既存 `reveal_current_find_match` / disclosureの経路で対象を確認できるようにする。

## 6. 全置換の準備・適用・履歴

### 6.1 準備と取消

実行時に確定したReadyの完全な一致一覧を対象にする。`truncated`なら全置換を無効化し「100,000件を超えています。対象を狭めてください」と表示する。ちょうど100,000件で打切りなしなら実行可能。上限到達の表示を「総数100,000件」としない。1件置換は確認済みのcurrentが有効なら使用できる。

共有可能なrope snapshotと照合済みの一致一覧から、変更計画をbackgroundで作る。元sourceと同じbytesの一致は計画から除き、実変更件数を数える。一致を元文書の座標順に一度ずつ処理し、挿入した文字列をその全置換の途中で再検索しない。`a→aa` を繰り返すループを作らない。

既存検索の「実行中1job + 最新の保留request」制約を保つ。置換準備にも無制限にjobを増やさず、取消tokenと世代で失効させる。準備中は操作状態と取消手段を表示する。本文編集・文書切替・条件変更・scope変更・バー終了・閲覧モード移行は準備を取消/失効させる。準備中の本文編集自体を長時間ロックしない。

### 6.2 適用の契約

適用直前に §4 の全identity、current対象、編集可否、IMEなし、取消なしを再確認する。不一致なら計画を捨てて再検索し、古い位置をrebaseして適用したり、利用者に知らせず全置換を再実行したりしない。

全件分の範囲境界、順序、非重複、元bytes、結果サイズのchecked arithmeticを検証してから公開する。エラー・取消時は本文、selection、history/redo、revision、dirtyを実行前のまま保ち、件数を成功として表示しない。適用途中にawaitや画面更新・保存通知を挟まない。取消が可能なのは公開前までとし、公開済みなら実変更件数を表示してUndoを案内する。

内部表現の選択は実装担当に委ねる。既存単一範囲APIへ渡す最初〜最後の一致を包含する置換文字列を準備する方法も、複数範囲を一履歴として扱う方法も可能だが、以下をすべて満たすこと。

- `replace_range_recorded` を一致数分呼んで個別のUndoにしない。範囲間の元文章を変えない。
- 直前・直後の通常入力とは別のtransactionにし、全置換を1回のUndo/Redoで全文・selection方向まで復元する。0件・同一bytesだけならredoも破棄しない。
- 失敗し得る準備を変更前に済ませる。複数編集を選ぶ場合、途中失敗のroll back自体の失敗に依存しないよう、非公開candidateに適用・検証してからまとめて公開する。Undo/Redoの複数適用にも同じ原子性を持たせる。
- documentのrevision/delta、行mirror、parse・presentationが矛盾しないようにする。複数内部編集を使っても中間文書をsession・保存・描画へ公開しない。巨大な編集でdelta保持上限を越える場合も、古い位置の流用ではなく既存の再構築経路へ安全に移る。
- 全文を一致ごとにString化・複製しない。包含範囲方式は疎な一致でも大きな未変更区間を複製し、既存 `RopeBuffer::edit` も逆編集・行mirrorを準備するため、backgroundの文字列生成だけで応答性を満たしたとは扱わない。大量一致時の適用・Undo/Redoも測定し、止まる場合は非公開candidateの準備と短い公開処理にする。

新しいbatch/history APIを追加する場合は、変更計画、expected revision、編集前後selection、失敗時に未変更であることが分かる契約を持たせる。session identityやUIのfocusはeditor層に持ち込まない。既存構造編集・IME・通常入力の履歴を同時に大きく作り直さない。

### 6.3 成功通知

成功時だけscopeを新revision/長さへ更新し、選択は最後に変更した一致の挿入末端へ置く。`after_input` に一度通知して、dirty、通常保存、自動保存、下書き、H1による名前同期、検索・parse・表示の無効化を既存経路で行う。独立したファイル書き込みは追加しない。0件のときは編集通知を出さない。

完了表示は「文書全体: 12件を変更」「選択範囲内: 3件を変更」のように実変更件数を出す。同一bytesへの置換が混じる場合は「一致5件・変更3件」と区別する。再検索中は以前の一致件数を新しい件数として使わず、Ready更新待ちを表示して連打による二重適用を防ぐ。

## 7. 実装順と提出内容

1. 範囲走査と置換計画をeditor層に追加し、既存find APIとの比較・境界・取消のテストを先に固定する。
2. 原子的な記録付き適用とUndo/Redoを実装し、通常入力・構造編集・IMEの既存回帰を確認する。新APIが必要なら失敗前後の状態比較を含める。
3. UI状態、scope取得/失効、job世代照合、両欄のfocus/IME/action遮断、1件/全置換を接続する。既存検索だけの動作を保つ。
4. 保存・再読込、両OSのfocused GUI、大文書の測定を行い、current headと必要なbase contextへ結び付く証拠を実装PRに残す。

分割PRにする場合も上記は依存順であり、途中フェーズのmergeだけで#589をcloseしない。実装PRには選んだ履歴・原子性の方法、その理由、変更パス、実行した検証と未実施、測定条件・結果を記す。本設計用PRを実装用へ無断転用せず、採用後の設計をbranchから読める形で実装担当へ渡す。

## 8. 受入条件と検証の対応

| Issueの条件 | 自動テスト・観測内容 | 必要なGUI/実機確認 |
| --- | --- | --- |
| 文書全体/選択内の1件・全件・空文字置換、範囲・件数 | ASCII/日本語/絵文字/結合文字、長さ増減、大小文字、隣接/非重複、正逆selection、範囲端でまたがる一致、0件/同一bytes/混在。結果全文を固定期待値と比較する。 | 対象範囲・一致・変更件数と次currentを確認。Markdownの隠れた記号/リンク先も原文開示で確認。 |
| 一回のUndo/Redo、通常入力・対象外・他文書の保護 | `通常入力→全置換→通常入力→Undo×3→Redo×3` の各全文・selectionを確認。1件の独立履歴、no-op時のredo維持、本文/欄の履歴分離も確認。 | 保存して開き直し、H1置換時の既存名前同期、自動保存/下書きが途中状態を保存しないことを確認。 |
| 古い結果・境界・取消の保護 | 同revisionの別session、同sessionの別generation、編集、Undo/Redo、検索条件/置換値/scopeの変更と戻し、current変更、close/reopen後の遅延結果を注入。どれも本文を変えない。準備途中エラー/取消、無効範囲、サイズ計算失敗で全文・selection・revision・履歴を比較する。 | 準備中に本文編集/文書切替/取消し、旧成功・旧範囲が流用されない。閲覧モードでは実行不可。 |
| 上限とscope独立性 | 99,999 / 100,000 / 100,001件、範囲外だけ大量一致、範囲始端前からの重なり（例 `aaa` の `[1,3)` で `aa`）、窓/chunk境界、CR/LF/CRLFを含む本文を検証。 | 上限超過は総数や全件成功と表示せず、scopeを狭めて操作を再開できる。 |
| macOS/Windowsの入力・IME・focus | `document_find.rs` と `input_routing.rs` の既存GPUIイベントテストを拡張。両欄のEnter/Escape、Tab、貼付け、Undo/Redo、本文復帰を実event経路で検証。 | 両OSで日本語変換中Enterが置換せず、Escapeが取消だけを行う。ボタンblur時の変換、欄のUndo、本文へ戻った後の通常編集も確認。 |
| 大文書・大量一致の応答性 | background準備の取消、job上限、全文の反復複製がないことを確認。既存find・history・session・保存関連の回帰とrequired CIを実行。 | 同一fixtureの少数/大量/上限超過を測り、準備中の入力・取消・切替、公開時とUndo/Redoの停止時間、追加RSSを確認。 |

中心GUIシナリオは合成文書で「日本語を検索→複数段落を選択して取り込み→選択範囲だけ全置換→Undo→Redo→保存・再読込」。対象外段落と別タブは同じbytesのままにする。macOSとWindowsのnative IMEはそれぞれ実環境で確認し、GPUI合成eventの成功だけで実機確認済みとしない。

実装時の検証コマンドの基点は `cargo test -p hane-editor`、`cargo test -p hane-document`、`cargo test -p hane-session`、`cargo test -p hane-ui document_find` と `cargo test -p hane-ui input_routing`。新テストが別モジュールならそのfilterも実行する。最終的に現行CIのworkspace tests/Clippy等を満たす。GUI担当は `.agents/skills/hane-gui-test/SKILL.md` と現行GUI runbookに従って証拠を残す。

性能確認は既存benchmark/metricsの方法を使い、変更前mainと実装head、fixture bytes/一致数/選択範囲/置換文字列、build構成、OS、raw dataを記録する。少なくとも巨大単一行、疎な一致、大量一致、上限超過を区別する。今回のために新しい計測基盤や全機能再検証を必須にしないが、準備・適用・Undo/Redoのどこが重いかを未測定のまま応答性達成としない。

正規表現、複数行の検索・置換、複数ファイル置換、変更差分previewは対象外。検索方法のUnicode正規化・case folding変更、保存形式変更、入力/履歴の全域リファクタリングも混ぜない。実装で契約の矛盾が見つかった場合は、要求の正本と本設計を整合させてから進める。
