# Issue #617: サイドパネル上部のピン留め — 実装担当者向け設計

## 1. 契約と今回の状態

要求・受入条件の正本は [Issue #617](https://github.com/hide212131/hane/issues/617)。本書は具体的な実装接続・状態・検証の設計である。調査基準は main `32f34f9cdc774a5f5b7ddad986eeca6f62bfc3cd`、2026-10-25 JST。

準備時点の履歴として、この準備commitは文書だけであり、製品実装・テスト・build・実GUI確認は未実施だった。当時の利用者の依頼はIssue・実装担当者向け設計・ブランチ／PRまでで、Claude起動依頼は投稿していなかった。後続の実装・検証の現在状態は、この文書ではなくcurrent Issue、PR head、関連runを参照する。実装開始時にcurrent main、PR head、関連PR/runを取り直す。別の設計採用PRを実装用へ転用するものではない。実装開始と受入・mergeの判断はdefault branchのCommander Policyに従う。

ChatGPT風という参考は、利用者が指定した「上部の専用エリア」「ホバーで操作ボタン出現」「クリックで登録・解除」を具体化する意味で使う。ChatGPTアプリの現行内部仕様・寸法を調査して同一性を保証する設計ではない。

## 2. 見た目と操作

```text
[サイドパネル開閉]               既存
[新規メモ] [新規フォルダ] [本文検索]
[通常一覧の並べ替え] [名前フィルタ]

ピン留め                         0件なら領域ごと非表示
  📁 設計                  [解除] ← 行ホバー／ボタンfocus時
  📄 会議メモ              [解除]
────────────────────────────
作業フォルダ                     既存root行以下の通常ツリー
  📁 設計                  [解除] ← 登録状態は両方で共有
  📄 日報                  [ピン]
...
[設定]                           既存footer
```

- 開閉行・toolbar類は現状の並びを保ち、ピン領域は通常リストのroot行より上へ挿入する。本文検索モードには挿入しない。検索から通常表示へ戻ると復元する。
- 各通常ファイル／フォルダ行の右端に24px程度の操作枠を常に確保し、非ホバー時はアイコンだけを隠す。名前は省略表示し、ホバーで名前やアイコンの位置がずれない。pin/unpinの既存アイコンがなければ `icons.rs` と既存asset方式に合わせて追加する。
- 操作名は「ピン留め」「ピン留めを解除」。tooltipとaccessibility labelを付け、ボタンのkeyboard focus時も表示する。ホバー自体は読み取りだけで、クリックで一度だけ状態変更する。登録済み通常行も解除ボタンを表示する。
- ピンの新規登録は先頭へ追加。既存項目への二重登録はno-op。解除→再登録は先頭。ファイル／フォルダを同一の順序付き一覧にする。通常sortや更新日時でピン順序を変えない。
- ピン行に対象名・種別を表示する。同名対象は相対親パスの補助表示またはtooltipで判別できるようにし、tooltipには相対パス全体を示す。
- ファイルの行クリックは `open_work_folder_entry` へ渡す。同じ文書は既存session/tabを再利用し、別の読込や文書コピーを作らない。
- フォルダの行クリックは、祖先と対象を `expanded_folders` へ加え、`selected_folder` / `SidebarFocus::Folder` を既存経路で更新し、通常ツリーの対象行までscrollする。既に展開済みでも閉じない。名前フィルタ中は既存の入力保護を確認してフィルタを解除してから移動する。これはroot切替や別window起動ではない。新規メモ作成の既存保存先選択にも同じ選択を使う。
- ピン行のダブルクリックでrenameを開始しない。rename等は通常ツリーの既存経路を使う。第二の子ツリー・コンテキストメニュー体系は追加しない。
- 対象は現在の `WorkFolderNode` として扱えるMarkdownファイル／フォルダ。root、未保存draft、root直下 `.hane`、作業フォルダ外、scannerが対象外とするsymlink等は登録させない。

### 高さ・スクロール・フィルタ

ピン領域は通常ツリーのスクロールと分離し、通常ツリーをscrollしても上部に残す。見出しを含む高さは「残り一覧領域の35%」と「見出し＋5行相当」の小さい方を上限とする。超過時は内部scrollで全ピンへ到達できるようにする。通常一覧に最低2行相当を確保できない極端な低さでは、ピン領域も含めた一覧全体を縦scrollするfallbackとし、固定toolbar/footerが一覧を押しつぶす場合も利用可能なoverflow経路を設ける。特定の要素を画面外へ永久に閉じ込めない。

通常の名前filterはピン一覧を絞り込まない。ピンからファイルを開いてもfilterは保持し、フォルダへ移動するときだけ上記の解除を行う。未保存draftは従来どおり通常一覧に残す。通常scrollbarとピンscrollbarのwheel/dragを分離する。

`list_top` / `list_viewport_height` / `sidebar_list_content_height` とoverlay scrollbarのthumb・hit testは、実際のピン領域の高さを含めて一貫させる。ピンscrollのwheelで通常ツリーまで同時移動させない。パネル開閉でpin state・scrollを初期化せず、幅変更でボタンが隠れないことを確認する。

### 入力の不変条件

操作ボタンでmouse-down/click/double-clickが親行のopen・folder toggle・renameへ伝播しないよう、既存GPUIのevent propagation制御を使う。ボタン押下で本文caret、選択、undo履歴、active session、フォルダ選択を変えない。

IME未確定中、inline rename中／結果待ち、tab close確認中など既存の入力保護に抵触する状態では、登録・解除または行移動を受け付けず短い説明を表示する。pinのためだけに強制blur・未確定入力の確定／取消しをしない。一般のpin操作で本文編集や自動保存を起動しない。

## 3. 状態と永続化

既存 `crates/session/src/store.rs` の `StateStores` にpin repositoryを追加する。設定本体・recent filesとは別の保存データとし、`FileStateStore` / `MemoryStateStore` の両方を用意する。小さいpin用moduleの追加はよいが、汎用workspace managerや新たなwatcherは作らない。

論理モデル（型名は提案）:

```text
WorkspacePins { root_identity, entries: Vec<PinnedEntry> }
PinnedEntry { relative_path: PathBuf, kind: File | Folder }
```

`entries`の並びが表示順序の正本。名前・更新日時・本文・展開状態を永続データに複製せず、現在のWorkFolderから解決する。root identityは既存 `identity.rs` 等のOSに合うパス同一性の経路を確認して使い、UIの表示文字列で判定しない。ルート内の相対パスを保持し、同名の異なる親フォルダを区別する。root identityが異なれば別一覧。作業フォルダのルート自体をOSで移動した際の自動移行は初回の保証外とする。

保存先は既存のユーザーstate directory内の専用ファイル（例 `sidebar-pins.json`）。work folderの `.hane` には書かないため、読み取り専用の作業フォルダでもユーザーstateが書ければ登録できる。version付き形式でrootごとの配列を保持する。追加のserialization依存は既存workspaceの方針に合わせ最小にする。

- APIはworkspace単位のloadと更新を提供し、他rootの保存内容を呼出元の古いsnapshotで丸ごと上書きさせない。共有repository handleで通常の複数windowの更新を直列化し、同rootでも変更操作を最新の保存状態へ適用する。保存後に同rootの表示を更新できるようにする。独立した複数プロセスを跨ぐ分散同期は追加しない。
- 欠損ファイルは空として開始できる。破損・読込権限エラー・未知versionは空で正常復元したことにせず、警告を出し、元ファイルを保ったまま書込を抑止する。将来形式を空データで上書きしない。
- パスをlossy文字列化して別名と衝突させない。選ぶ形式でnative pathを保存できなければ、対象の登録を説明付きで拒否する。空・絶対・親参照でroot外へ出る相対パス・禁止管理領域・kind不正・重複を検証する。
- I/Oはrenderやhover内で行わない。既存 `atomic_write_bytes` と同等の原子的書込を背景処理で利用する。明示pin/unpinは保存成功後に表示へ確定し、処理中の重複クリックを抑止する。失敗時は以前の表示と保存状態を維持し、日本語で保存失敗を示す。
- `EditorView`には現在rootの解決済みpin state、読込／保存中情報、専用scrollだけを持たせる。新しい要求にはrootと `work_folder_generation`、同root内で更新が逆転しないrequest識別を関連付ける。root切替時は旧表示を先に外す。旧要求の完了が旧rootの保存を終えても、新rootのUI・statusへ適用しない。
- `StateStores::new` の既存callerとテストを確認し、互換constructorを維持するか全callerを明示的に追従する。pinを使わないwindowのために既存設定loadを壊さない。

## 4. 名前変更・削除・外部変更

| 事象 | pinへの反映 |
|---|---|
| Hane内ファイルrename成功 | 旧pathを新pathへ置換し、順序維持。未openのファイルも対象。 |
| フォルダrename成功 | 自身と配下pinをpath component単位でprefix置換。`a`と`ab`を混同しない。重複時は元順序で一件へ集約。 |
| H1連動自動rename成功 | `finish_title_rename` の成功受理点で同じ更新を適用。 |
| Save As | 元ファイルの存在が残るコピーならpinは元pathへ残す。pinがあるだけで新pathへ移さない。 |
| Hane内trash成功 | 対象と、フォルダならその配下のpinを除去。cancel／失敗時は保持。 |
| 外部削除／外部rename | 完全な成功scanで解決不能になったpinは「見つかりません」として残し、openを拒否し解除可能にする。外部rename先のidentity推測はしない。利用者は新位置を再登録できる。 |
| 外部追加／同path再出現 | 成功scanで同kindの対象が確認できれば再び利用可能。同path別kindなら自動転用しない。 |
| scan失敗・監視再接続中 | 最後のpin登録を保持。失敗を空一覧・全件削除と解釈しない。 |

`finish_work_folder_refresh` 等の既存freshness guardを通過したcurrent listingだけでpinの可用性を更新する。pinのための追加全走査・本文読込・定期pollingは作らない。render内でパスごとにstatしない。クリック時の存在／種別・root内検証は既存open等のI/O境界で行い、scan以降の消失も安全にエラーへする。

製品のrename/trash成功とpin保存は別のI/Oであり、pin保存だけ失敗しても製品ファイル操作を「失敗したので再実行」と誤表示しない。メモリ上は成功後の正しいpathへ追従／除去し、登録情報の保存に失敗したことを警告し、未保存更新を保持して再試行できるようにする。次回pin更新で古いpathを復活させない。既存statusを直後の「Renamed」等で上書きして警告を消さない。

## 5. 接続箇所と並行開発

調査時の経路。実装者はcurrent branchの配置を再確認する。

| パス・シンボル | 変更責務 |
|---|---|
| `crates/ui/src/view/sidebar.rs`: `work_folder_files_sidebar` | 上部エリア、行hover、登録/解除button、height/scroll、通常行との共通操作部品。既存file/folder行handlerの伝播防止。 |
| 同: `sidebar_context_trash` / `sidebar_context_trash_folder` | current generationの削除成功受理後にpin整理。 |
| `crates/ui/src/view.rs`: 初期化、`finish_work_folder_scan`、`switch_to_work_folder`、`finish_external_folder_open` | state注入、root切替、load、既存generationを利用した結果受理。 |
| 同: `open_work_folder_entry` / `toggle_and_select_work_folder_folder` | 既存openを利用。pinフォルダの明示展開・選択・可視化は通常toggleと区別し、共有状態へ接続。 |
| 同: `sidebar_list_content_height` と `view/viewport.rs` | 描画高さ・thumb・入力座標の整合。親にあるhelperは必要範囲のみ調整。 |
| `view/folder_watch.rs`: `finish_work_folder_refresh` | 受理済みlistingでpinの解決状態だけを更新。 |
| `view/inline_rename.rs`: `finish_inline_rename` | rename成功時のpath置換。既存session・recent・draft調停を保持。 |
| `view/session_save.rs`: `finish_title_rename` | H1連動rename成功時のpin追従。 |
| `crates/session/src/store.rs`、必要ならpin専用module、`lib.rs` | 順序付きモデル、workspace別保存、in-memory実装、エラーとupdateの契約。 |
| `crates/ui/src/icons.rs` と既存icon assets | 必要なpin/unpin iconと表示サイズ。 |

#567 / PR #613 はpanel drag、本文autoscroll、sidebar thumb寿命の機械移動。pin機能へその整理を混ぜず、実装開始時にmainへ受入済みの配置を使う。#608 / PR #610 はcontext menuとdraft保存先の変更。同じ行・選択状態へのhookを最小範囲で調整し、未merge実装を独自に再現しない。#554の監視、#585のtrashは現在mainに存在する経路を使う（Issueのopenだけで未実装と断定しない）。#598のdrag・#237のキーボード機能を今回実装しないが、行IDは通常/ピンの表示箇所＋対象identityで一意にし、将来のeventを取り違えない。

全体RF #297・#298の完了待ちは不要。shared stateの競合が現れた場合だけ、その責務の最小整理を別PRで先に受け入れる。pinの新しい判断ルールを通常行とピン行へ別実装で二重に置かない。新ADR・汎用manager・独自workflowは不要。

## 6. 実装順序と検証対応

1. 実装開始時のhead/baseを保存し、既存のsidebar open・folder selection・filter・rename・watch・trashのテストを確認する。通常の一覧scroll／描画をpinなしで測定し、同じfixtureとviewportで後から比較できる基準を残す。
2. pinモデル・保存・エラー・memory repositoryを実装し、順序・identity・path変換のunit回帰を追加する。
3. Viewのload/update・root寿命、rename/trash/watchの成功接続を実装する。
4. 行操作button・上部エリア・folder reveal・geometryを接続し、実イベントで誤伝播を検証する。
5. focused testsと両OS CI・GUIを実施し、IssueのACに対応する対象SHA・base context・結果をPRへ残す。

| 検証 | 必須ケース |
|---|---|
| session unit / repository | add先頭・dedup・unpin・再登録・reload・root A/B分離・同名別親・native path・不正path・未知version・読込/書込失敗・他root更新保持・同rootの連続更新 |
| path更新 unit | file/H1 rename・folder prefix・配下のみ更新・順序・重複・trash成功/失敗・Save Asで元pin保持 |
| GPUI event | pointer移動だけでは変更なし。実mouse-down/up/clickでpin/unpin各一回、親open/展開/renameなし、別rowにhoverが漏れない、両表示が一致、keyboard focusでbutton表示 |
| View寿命 | A load/save中にBへ切替、同rootの連続操作、旧scan/旧watch結果拒否、pin保存の失敗表示と文書操作成功の区別 |
| 表示・入力 | narrow幅・長い日本語名・低い高さ・0/1/多数pin・独立scroll・thumb hit test・filter中file open/folder reveal・検索から復帰・パネル開閉・IME/rename保護 |
| 文書保護 | 既存tab再利用、未保存本文・caret/選択・Undo/Redoがpin操作で変化しない。消失pinが新文書を作らない |
| 外部変更 | 成功scanでmissing状態、再出現、kind変更、失敗scanでは保持。本文を読み込まない |

最初に `cargo test -p hane-session` と `cargo test -p hane-ui` の変更領域をfocused実行する。提出時はrequiredなmacOS/Windowsのworkspace tests、Clippy、変更に必要な通常構成とall-featuresを確認する。例: `cargo test --workspace --all-features`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`。commandの列記を実行証拠にしない。

### 両OS focused GUI

隔離stateとfixtureを使用する。例: root Aに `設計/会議.md`、`別/会議.md`、root Bに別の `会議.md`、多数のpin候補。未保存本文を別tabに用意する。

1. ファイルとフォルダにhover→button表示→登録。上部へ表示、通常ツリーにも残る。hoverを離しても登録維持。buttonクリックで本文やfolder展開が変わらない。
2. pinからファイルを開いてtab再利用、folderから通常treeへ移動。filter中にも同様に実行。上部・通常tree双方から解除して双方へ反映。
3. 狭い幅、低い高さ、多数pinでwheel/scrollbar/resize。通常treeとpinを独立に操作でき、設定も利用できる。
4. file・folder・H1連動renameとtrashを行い、配下pin・順序を確認。外部削除後にmissingから解除、再走査失敗時の保持を確認。
5. A→B→Aと再起動で分離・順序復元。操作前後で未保存本文とUndo/Redo、IME未確定入力が保たれることを確認。保存失敗は隔離storeの失敗注入で確認してよい。

GUI Validatorは製品コードを変えず観測を提出する。macOSは既存の正規GUI経路、Windowsは接続先Windows上のChatGPTアプリから直接操作し、Mac経由VM操作を使わない。失敗時は `docs/gui-test-recovery.md` に従って診断・復旧して元scenarioを再実行する。検証アプリの実PID・実行パス・隔離state・対象head/baseを記録し、終了確認後に片付ける。未実施やunknownをpassへ置換しない。

### 性能と復旧

pinなしの既存fixtureに加え、同じ作業フォルダで多数pinを持つ条件をbefore/afterで比較する。hover/pinで全走査・本文loadが増えないこと、render内I/Oがないこと、通常treeのscroll/geometryが回帰しないことを確認する。数値の採否は既存の性能契約を使い、新しい任意閾値は設けない。

pinデータは専用ファイルで、導入前はファイル欠損として扱える。rollbackで本文・作業フォルダへ変更は不要。製品コードをrevertしてもpin保存ファイルは削除せず、再導入可能にする。破損データも勝手に消さない。

## 7. 判断記録

Jev `typesafe` / 要求 `jev-latest` / 実行 `jev-1.14.0` は、Issue発行 `create_issue`（0.87）、仕様 `shortcuts`（0.99）、次の文書準備 `prepare_design`（0.93）を選択した。Commanderはmain・既存source・重複なし・関連PR・作業権限を確認した。これらは設計時の判断記録であり、実装・受入完了の証拠ではない。
