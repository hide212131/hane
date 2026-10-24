# Issue #607 実装準備: Finder / Explorer からフォルダを開く

- Issue: https://github.com/hide212131/hane/issues/607
- 基準 main: `68c438aea3ce39e12b25aa4ad3e2f5e16294190d`
- 実装 branch: `codex/issue-607-folder-open-prep`
- 関連: #591（画像のOSドロップ）、#598（sidebarから本文へのdrop）、#603 / PR #604（本文内link click）、#585（sidebar folder trash）

このcommitはClaude実装用Draft PRの準備資料だけを追加する。製品コード・テスト・bundle設定は変更しておらず、build / test / GUI確認も未実施。

## 目的と受入契約

Finder / ExplorerからHaneへフォルダをドロップするか、フォルダのコンテキストメニューから「Haneで開く」を選ぶと、選択したフォルダをWork Folderとして開く。既存Markdownファイルの起動・drop・右クリック動作、単一window、非同期folder scanを維持する。通常起動用の`default_folder`は変更しない。

起動中windowにdirtyまたは保存中のsessionがある場合は、外部folder切替でsessionを破棄しない。失敗・拒否時に既存sessionと編集内容を保ち、古い非同期結果を後から適用しない。

## current mainで確認した入口と責務

- 外部file dropは`crates/ui/src/view/render.rs`のroot `on_drop`から`crates/ui/src/view.rs`の`on_external_paths_drop` → `handle_external_drop` → `apply_external_drop`へ届く。現在は空 / 1件 / 複数件を分け、1件は常に`open_path`へ渡すため、folder dropはWork Folder切替にならない。
- Finderの`on_open_urls`は`crates/app/src/main.rs::application_with_finder_open`で登録される。`crates/app/src/finder_open.rs::markdown_path_from_url`はlocal `.md` URLだけを受け付ける。folder用URLを安全に受け、Finder serviceと既存queueから通常のexternal-open入口へ渡す必要がある。
- `EditorView::open_work_folder`と`switch_to_work_folder`は既存のWork Folder scan経路を使う。scanはbackground executor上で行われ、generationが古い結果を拒否する。
- `EditorView::prompt_open_work_folder`はdirty sessionを拒否するが、`open_external_path`のdirectory分岐はそのguardを通らず`switch_to_work_folder`を呼ぶ。external folder switchにもdirty / save-in-flight保護が必要。
- `crates/app/src/main.rs`のstartup pathはdirectoryを`EditorView::open_work_folder`へ渡す。Windowsのrunning-instance入口は`crates/ui/src/view/app_update.rs::submit_external_open`から同じ`open_external_path`へ届く。

## OS連携

### macOS Finder

- `scripts/bundle-macos.sh`が作る`Contents/Info.plist`は現在Markdown UTIだけをdocument typeとして宣言し、同scriptが「一種類のみ」「既定handlerにしない」を検査する。
- folderを通常のdocument typeとして登録して既定アプリ候補を変えるのではなく、Finderで選択したfolderを明示的にHaneへ渡す標準のcontext actionを追加する。FinderのServices機構は`NSServices`、Finder限定の`NSRequiredContext`、file typeの受け渡しを提供する。実際に選択folderがアプリqueueへ届き、起動中Haneにも届くことを確認する。
- AppleはFinder Syncを同期アプリの状態表示・操作向けに説明し、Finder UIを一般目的に変更する拡張としては意図していないとしている。必要がない限りFinder Sync extensionを新設しない。
- 参考: [Services Properties](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/SysServices/Articles/properties.html)、[Finder Sync](https://developer.apple.com/documentation/FinderSync)

### Windows Explorer

- `crates/ui/src/context_menu.rs`には既存の`ExplorerVerb::Folder`登録があり、Explorerのfolderコマンドからpathを渡す。
- `docs/windows-explorer-integration-verification-20260924.md`には実Explorerのfolder menuを確認した証拠がある。登録・ON/OFF・既定アプリに関する設定を重複実装せず、folderがcold start / running instanceのどちらにも届く経路を維持する。

## 並行作業との関係

- #591は画像データ、#607はdirectory pathを扱う。どちらもOS外部dropの入口を使うため、path種別のdispatchは一箇所に保ち、今後のfile/image routeと相互に消し合わないこと。#591には現時点でopen実装PRがない。実装開始時にIssue / PRとcurrent mainを再確認する。
- #598はsidebar内のMarkdownを本文へdropして相対linkを挿入する別入力・別結果。既存external `FileDropEvent`を保つ。
- PR #604は`EditorView::apply_row_click`内の本文link activation。current headでreview中だがexternal path、folder state、drop handlerと別責務であり、#607はそれらだけを変更する。
- #585はsidebar folderのTrash操作であり、先行条件ではない。#297 / RF0全体の完了にも依存しない。

## 実装時の確認

Issue本文の全acceptance criteriaを維持する。特にFinderのcontext actionがフォルダ選択時に実際に表示されること、file URLの不正/remote authority・非Markdown file・空白/日本語pathの扱い、複数drop、file動作回帰、dirty / save-in-flight session保護、window state、stale scanを確認する。macOSのbundle scriptが持つdocument-type制約を無言で緩めず、Finder「Open With」やdefault folder設定へ影響させない。

直接関連する回帰テストを追加し、macOSとWindowsのfocused GUI scenarioはCommanderが別途実施する。準備commitではbuild / test / GUIを実行していない。
