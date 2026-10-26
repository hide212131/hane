# Issue #623 Phase 4: プロキシ設定UI

状態: 実装用Draft PRの準備文書。これは作業契約であり、製品コード・テストの実装完了を意味しない。

## 基準と正本

- 基準main: `c843a2d1a357a766ef2a3bd3750d2d4476e90c3f`（PR #620 merge後）
- Issue: [#623](https://github.com/hide212131/hane/issues/623)
- 詳細設計: `docs/design/http-proxy.md` §3、§4.1、§7〜§9
- 既存の設定画面経路: `crates/ui/src/view/settings_screen.rs`（#620で分離しmainへ反映済み）
- 共通型と契約: `crates/network/`。設計・IssueのP1〜P10を全体受入条件とする。

## 今回の目的

macOS / Windowsの設定画面から、アプリ全体で使うプロキシ方式を確認・編集し、安全に保存できるようにする。環境変数を使う（初期値）・直接接続・手動HTTPプロキシを選べるようにし、既存の共通network契約へ接続する。

## 今回の範囲

- #620で作られた `settings_screen` 経路へ「ネットワーク」設定を追加する。親Viewへ設定UIを戻さず、既存の設定カテゴリ遷移・dirty state・エラー表示の流れに合わせる。
- `NetworkSettings` / `ProxyMode` 等の共通型を使い、現在値と入力中の値を分けて編集する。
- 環境変数 / 直接接続 / 手動HTTPプロキシの3方式、manual endpoint、除外先、認証なし / Basicを表示・編集する。Basicのusername/passwordは別入力、passwordはマスクし、HTTPプロキシまでの通信が暗号化されない旨を表示する。
- scheme、host、port、userinfo、path/query/fragment、制御文字、除外先などの入力検証を設計どおり行う。HTTPS/SOCKS、PAC/WPAD、NTLM/Kerberos/Digest等は未対応として明示し、暗黙にDirectへ切り替えない。
- `crates/session/src/store.rs` の既存settings保存を拡張し、`network_*`設定を保存・再読込する。既存設定/default folderを保持し、旧設定の未指定値はEnvironment方式として移行する。未知schema・壊れた値を正常なDirect設定として扱わず、設定エラーとして保持・表示する。
- password等の秘密値をsettings.conf、通常ファイル、ログ、診断、argvへ書かず、OS秘密ストア用の `CredentialStore` とservice `hane.network.proxy` を使う。保存は「新しい秘密の保存成功 → 設定のatomic保存 → 旧参照の削除」の順とし、途中失敗では旧設定を有効のまま維持する。
- 設定を開く、入力を編集する、保存するだけでは通信を開始しない。保存後は明示的な次回接続で新設定を使う。設定画面に必要な保存・取消・失敗・再接続案内を実装する。
- UI/state/secret境界のunit・fixture testsを追加し、macOS/Windowsのfocused GUIで表示、入力、保存・再読込、取消、入力拒否、秘密保存失敗を確認する。実施していないOSのGUIはUnknownとして明記する。

## 対象外

- 共通route解決、環境変数優先順位、除外先判定の仕様変更（Issue #623実装順1 / PR #625）。
- 更新HTTPS CONNECT、curl、署名・redirect・download制御（実装順2 / PR #628）。
- pinned AI runtimeの環境、再接続/generation、更新後のrestart継承、P7〜P9の実runtime検証（実装順3 / PR #631および後続検証）。
- 外部ブラウザ・外部エディタ内部のproxy設定、HTTPS/SOCKS proxy、PAC/WPAD、自動OS proxy検出。
- Issue #623 P1〜P10全体の完了宣言、親Issueのclose、別Issueや他PRの修正。
- `.github/`、AADW/Commander Policy、AGENTS等の運用保護パス変更。

## この段階の受入条件

1. macOS / Windowsで設定画面からNetwork設定へ到達でき、3方式を現在値として表示・編集できる。
2. Manualのendpoint・bypass・認証入力を設計どおり検証し、不正値を保存しない。未対応schemeは未対応と表示する。
3. 設定を保存・再起動後に再読込でき、未指定の既存設定はEnvironmentへ移行し、既存設定を壊さない。
4. 秘密はOS秘密ストアにのみ保存し、秘密保存・設定保存の失敗時に旧設定が有効なまま残る。エラーを成功表示や認証なし接続へ読み替えない。
5. 起動・設定表示・入力・保存・取消だけでは外部通信を行わない。
6. 入力中の値、保存済み値、保存失敗を区別でき、画面を離れるときに未保存変更を失わない。
7. workspace tests、Clippy、通常Release buildを実行し、その結果を記録する。P1〜P10のうち、このPRが満たした条件と後続段階に残る条件を明確に分ける。
8. 実機GUIのhead/base・OS・シナリオごとの結果を記録する。UnknownはPassにしない。

## 実装上の制約

- 文書上の仕様は `docs/design/http-proxy.md` とIssue #623を正本とし、実装で黙って狭めない。矛盾が見つかったら先にCommanderへ報告する。
- UI側はsettings screenの分離後のmoduleを使う。Windows UI thread stack制約と既存の設定dirty-state/leave guardを維持する。
- UIテストは秘密値をassert出力・snapshot・ログへ出さないfixtureにする。実ユーザーの資格情報を使わない。
- この準備PRはこの文書だけを変更する。製品コード、tests、commit追加実装を含めない。
- 実装用Claude handoffは、この文書が実装PRの対象branchから読め、current head/base・同一Issueのactive run・重複handoffを確認した後に一度だけ行う。
