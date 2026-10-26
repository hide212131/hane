# Issue #623 P1: HTTPプロキシ共通契約の実装準備

対象Issue: #623。採用設計: `docs/design/http-proxy.md`（設計PR #624）。基準mainはこの文書の末尾に記録する。

## 今回の範囲

Jevは設定UI分離PR #620との重複を避け、設計§8の実装順序の最初である共通coreを選択した。新規 `crates/network/`（package `hane-network`）とworkspace依存を追加し、後続の更新/curl/AI/設定保存が同じ契約を使えるようにする。設計全体をこの部分へ縮小しない。

- 設計§4の `ProxyMode`、`NetworkSettings`、`ProxyEnvironment`、`ResolvedRoute`、構造化エラーと秘密参照の型を作る。設定のinvalid状態をDirectへ変換しない。秘密値にSerializeを実装せずDebugは伏せる。
- resolveはI/Oなし。環境mapとOSの大小文字ポリシーを注入し、実環境の変更、DNS、通信、OS秘密ストアアクセスを行わない。route決定とcredential取得を分離し、除外宛先で不要な秘密取得を起こさない。
- 環境HTTPSはhttps_proxy→HTTPS_PROXY→all_proxy→ALL_PROXY、HTTPはhttp_proxy→all_proxy→ALL_PROXY。macOSでは大文字のみHTTP_PROXYを無視、Windowsは名前case-insensitive。空値と未定義は未設定、非空の上位不正値はエラー。
- manual/environmentのendpointはHTTP proxyのみ、DNS/IPv4/[IPv6]と既定port80。manual URLにuserinfo不可、環境URLのBasic userinfoは一回decodeし制御文字を拒否。path/query/fragment/portの検証を設計通り実装する。
- bypassはcomma、空白、空要素、*、domain境界と末尾dot、IP完全一致を扱う。任意wildcard/CIDR/URL/port付きDNSは拒否。localhost、127/8、::1の宛先は常にDirect、localhost subdomainは除外しない。無効な除外一覧はエラー。
- 後続AI用の純粋なchild env adapterを作る。proxy系だけを除去し、選択したmodeから矛盾するALL_PROXY等を残さず正規化する。証明書/PATH/provider値は変更せず、秘密をログへ出さない。実runtimeでの解釈一致は後続P7で検証し、今回env構築だけで通信対応済みとしない。
- credential参照と取得失敗の契約を定義する。Keychain/Credential Manager backend、永続化、更新再起動の一時秘密は後続。ネットワークcoreからAI/session/UI/updateへ依存させない。

## 今回の受入条件

上記の通常/境界/不正入力をtable testsで覆い、Direct/Manual/Environmentの選択、優先順位、IPv6、bypass境界、秘密Debug/Serialize禁止、取得失敗でDirectにしない契約を確認する。実process環境を汚染しない。既存workspace契約を壊さない。

変更許可範囲は `crates/network/**`、`Cargo.toml`、`Cargo.lock` と直接関連する文書に限定。Cargo.lockの整合が必要なら手編集で推測せず、worker結果にtrusted検証側での生成が必要な旨を記載する。shell/test/build/lint/commit/pushはworkerでは実行しない。

この準備commit時点では製品実装・test/build/lint/GUI/実通信は未実施。trusted finalizerによるpush後のexact-head verifyとmacOS/Windows CIを別に確認する。

## Issue全体の残条件

設計§9とIssue #623のP1〜P10を維持する。今回P2の純粋契約とP1/P10の基盤を実装するが、設定保存・OS秘密backend/UI（P1）、更新CONNECTと検証済みstaging（P3〜P5）、curl実通信（P6）、pinned runtime実通信（P7）、revision/停止再接続（P8）、更新後の安全な継承（P9）、起動無通信/GUI/診断（P10）は残る。部分PRをmergeしてもIssueをcloseしない。

## 対象外・並行作業

PR #620/#622のbranch/worktree、設定UIの旧経路、他の製品crate、vendor、pinned runtime変更、`.github/`、AGENTS/CLAUDE/Commander Policy、AADW運用設定は変更しない。#620が未mergeでも共通coreは独立して進められる。UIは受入済みsettings_screenへの後続実装とする。

基準main: `b9676139d450a60cf51857840d0ebf64d5f20c91`。設計PR #624のmerge commitと一致する。
