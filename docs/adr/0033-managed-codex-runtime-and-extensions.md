# ADR-0033: Hane専用Codexをダウンロード・管理し、拡張の読み込み元を明示する

## ステータスと範囲

**Proposed — 実装契約の提案。製品実装・配布・Windows/macOS実機検証は未完了。**

作成日: 2026-10-13 JST。親Issue: [#479](https://github.com/hide212131/hane/issues/479)。調査基準main: `af981629011f04f68b25c8b58e476eb3eeffa624`。公開版v0.26.0: `5c2e703334c66c09575b549030aa1ccb913920dd`。

本ADRは[ADR-0032](0032-embedded-codex-app-server-ai-foundation.md)のうち、§4.1「同梱sidecar」としたランタイム配布決定を**初回ダウンロード方式へ置き換える**。Hane配布物へruntime実行ファイルを入れない点を新しい正とし、それ以外のAI接続、認証、stdio protocol、所有権の決定は引き継ぐ。ADR-0032自体は当時の提案記録として残し、実装済み・検証済みとは変更しない。設計PRの承認・マージは、実装Issueの完了を意味しない。

## 1. 背景

v0.26.0の起動コードは、明示的な`HANE_CODEX_APP_SERVER`、なければHane実行ファイルに隣接するstandalone App Serverを選ぶ。一方、Windows配布処理にはCodexを追加する工程がない。今回の方針ではこの状態を保ち、初回AI利用時に利用者が明示的に取得を開始し、固定版をHane専用領域へ導入する。[H1][H2]

HaneのAI状態はアプリ専用領域にあり、ChatGPT用とCustom Provider用のCODEX_HOMEを分けている。子プロセスの環境変数は許可したものだけを引き継ぎ、文書フォルダーではなく専用の空作業フォルダーを使う。現在の生成設定は近傍のAGENTS・プロジェクト設定・ホストSkillsの自動取り込みを抑えている。[H3][H4]

旧[#380](https://github.com/hide212131/hane/issues/380)はnot_plannedで閉じられている。今回の要求をその完了証拠にはせず、新しいIssueで追跡する。

## 2. 決定案

### D1. 初回利用時に固定版の公式App Server完全パッケージをダウンロードする

対象はHaneが現在配布するWindows x64とmacOS arm64だけとする。HaneのZIPや`.app`にはCodex実行ファイルを入れない。設定画面の明示ボタンを押したときだけ、`openai/codex`の完全パッケージを固定URLから取得し、サイズ・SHA-256・内容を検査してから使う。通常のCodex CLI、他アプリの同梱物、PATH上のCodexは代替物にしない。

初期候補は`rust-v0.157.1`。取得URL、サイズ、SHA-256、展開許可一覧を詳細設計に記録する。初回のAI利用にはインターネット接続を必須とし、起動時にバックグラウンド取得はしない。これまでのmacOS単体バイナリ試験を完全パッケージやWindowsの証拠へ転用せず、実装担当が両OSで導入・内容検査・接続試験を行う。[O1][O2][H5]
### D2. ダウンロード済みの実行物はHane専用領域で導入・修復・更新する

P0（#480）は初回ダウンロード、専用領域への安全な導入、初期化確認、診断を実装する。P1a（#481）は破損した導入の修復と既知の正常版への復旧、P1bはHaneが署名した小さなカタログによる検証済み更新を実装する。上流のlatestへ無条件に追随しない。新しいWebサービスやマーケットプレイスは作らず、配布元はGitHub Releasesとする。

設定画面の操作はHane専用領域だけを管理する。実行中の実行ファイル、macOSの.app、システムのPATH、他アプリのCodexは更新しない。バージョンと実配置の識別子を分け、修復も新しいステージング先へ行う。切替は既存のAI操作が終わり、所有する旧プロセスの停止を確認してから行う。状態保存形式の前後互換を確認した組合せだけを更新対象にする。認証データの無断コピー・巻き戻しはしない。
### D3. 実行場所・認証・作業フォルダー・拡張の所有範囲を別々に扱う

実行ファイルの共有と認証の共有は別の問題である。Haneが管理する実行ファイルをChatGPT/Customの両接続で使っても、それぞれのCODEX_HOMEは引き続き分離する。既存の秘密保存方法・設定保存形式は今回の配布対応では移行しない。

Haneの停止対象は自分が起動したプロセスとその管理下の子だけとし、名前による全Codexプロセス停止をしない。実HOMEを残す既存設計を維持するため、完全なOSレベルの読み取り隔離を実現したとは表現しない。外部設定の不混入とOS資格情報ストアの分離を、採用版・OSごとに試験する。

### D4. 初期Skillsは、Haneが選択されたSKILL.mdの文章指示を読む

Hane専用`ai/skills`を設け、利用者が確認して有効化したスキルだけを選択文AIメニューから使えるようにする。初版は本文だけで完結する文章指示を対象とする。本文は構造化したユーザー入力へ添え、選択文そのものと区別する。system/developer指示へ昇格させず、スキルからアプリの権限を拡大しない。

これは**CodexネイティブSkillsの完全互換ではなく、SKILL.mdの文章指示を利用するHane側の限定対応**である。scripts/references/assetsや外部ツール依存のある構成は未対応と明示する。解析で任意の自然文依存を完全に判定できるとはしないため、有効化前にも対応範囲を表示する。

理由は固定版の契約差である。0.157.1の`SkillsListParams`には`cwds`と`forceReload`しかなく、最新資料の追加ルート指定を使えない。`UserInput::Skill`も既に発見されたスキル一覧から解決される。任意のパスを渡すだけで有効になるとは扱わない。[O3][O4][O5]

将来ネイティブ対応するときも、他アプリのSkills探索を一括で再開して解決しない。明示的な専用ルートとホスト非混入が両立する固定版を別途検証する。

### D5. 拡張Packageの初版はSkillsだけのローカル集合とする

`skills/<name>/SKILL.md`を含むフォルダー/ZIPを検査し、Hane専用領域へコピーする。外部ファイルを元の場所から直接実行せず、Package導入時にコマンドを実行しない。未知の実行設定や未対応ファイルを無視して成功にはしない。

このPackageはCodex本体の配布packageとも、Codex/ChatGPTの汎用Pluginとも異なる。初版の画面は「スキルパッケージ」と表示する。MCP・scriptsを伴う汎用Pluginは、この初版の完了条件に含めない。現在の公開App Server資料もPlugin管理APIに開発中の注意を付している。[O6]

## 3. 既存機能との関係

固定接続確認は従来どおり文書もSkillsも送信しない。選択文AIは既存の構造化入力・結果確認・取消・Undoの経路を使う。#449で認められた任意のツール使用を一律禁止する変更は行わない。スキルなしの要求は既存の振る舞いを保つ。

AGENTS.md、AADW/Commander Policy、レビューや実装担当の役割分担は本ADRの変更対象ではない。実装用Issueが指定する配布・CI処理の変更と、開発運用ポリシーの変更を混同しない。

## 4. ライセンスと信頼の扱い

Codexの採用タグのLICENSEはApache License 2.0で、条件付きの再配布を認めている。採用版のLICENSE/NOTICE、全同梱依存物の必要な表示、変更箇所の表示を配布物へ含める。トップレベルのLICENSEだけを見て、完全パッケージの確認が済んだとはしない。[O7]

取得物のハッシュは内容の一致を検査するもので、配布元の真正性そのものを保証しない。P0/P1aの取得情報はレビューしたHaneのソースに固定する。P1bの外部カタログはHaneに埋め込んだ公開鍵で署名を検証してから読む。ローカル管理者・同一ユーザー権限を持つ攻撃者によるHane本体の置換までを防ぐ設計ではない。

ChatGPT契約・API利用の条件と、OSS実行ファイルの再配布条件は別である。各利用者のログイン/キー設定は引き続き必要で、Haneへ認証情報を同梱しない。

## 5. 段階と受入条件

| 段階 | Issue | 利用者が完結できる操作 |
|---|---|---|
| P0 | [#480](https://github.com/hide212131/hane/issues/480) | ボタンで固定版をダウンロード・検証し、Hane専用領域へ初回導入してAI接続を開始する |
| P1 | [#481](https://github.com/hide212131/hane/issues/481) | Hane専用版を修復・復旧し、署名済みカタログから検証済み版へ更新する |
| P2 | [#482](https://github.com/hide212131/hane/issues/482) | 専用SKILL.mdを有効化し、選択文AIで使う |
| P3 | [#483](https://github.com/hide212131/hane/issues/483) | スキル集合をまとめて導入・更新・削除する |

P0を最初に実装する。P1とP2はP0のlocator/診断が確定すれば設計上は独立するが、個人開発ではP0→P1→P2→P3の順で一つずつ行えばよい。公開の拡張APIが未対応でもP0/P1を止めない。

詳細な型、保存形式、画面、競合、復旧、試験IDは次を正とする。

- [配布・導入・更新の詳細設計](../ai-runtime-distribution-implementation-spec.md)
- [Skills・Packageの詳細設計](../ai-extensions-implementation-spec.md)
- [実装担当への引継ぎ](../ai-runtime-implementation-handoff.md)

## 6. 退けた案と影響

上流latestを毎回取得する案は、Haneのプロトコル・設定・状態との互換性を確認できないため採らない。他アプリのCODEX_HOMEやauth.jsonのコピーは、ログアウトや更新時の干渉を生むため採らない。初回の固定版ダウンロードは、ADR-0032 §4.1の「Hane配布物に実行ファイルを同梱する」提案に代わる決定である。

初回利用にはネット接続が必要で、固定版の取得元・hash・展開内容を検査する必要がある。P1bには署名鍵の管理とOSごとの候補試験が必要になる。P2/P3は限定対応なので、既存Skills/Pluginsがすべて使えるわけではない。この制限はREADME、画面、エラーで一致させる。

## 7. 根拠資料（確認日2026-10-13）

Haneのリンクは調査時commitに固定する。外部の最新版仕様を固定版へ無検証で適用しない。

- [H1 起動コード](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/crates/app/src/main.rs)
- [H2 配布処理](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/.github/workflows/windows-release.yml)
- [H3 ランタイム](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/crates/ai/src/runtime.rs)
- [H4 生成設定](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/crates/ai/src/provider.rs)
- [H5 限定的な既存検証](https://github.com/hide212131/hane/blob/af981629011f04f68b25c8b58e476eb3eeffa624/docs/issue-379-t00-evidence.md)
- [O1 0.157.1公式配布物](https://github.com/openai/codex/releases/tag/rust-v0.157.1)
- [O2 同タグの完全パッケージ構築](https://github.com/openai/codex/blob/rust-v0.157.1/.github/actions/build-codex-packages/action.yml)
- [O3 同タグSkillsListParams](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/app-server-protocol/schema/typescript/v2/SkillsListParams.ts)
- [O4 同タグUserInput](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/app-server-protocol/schema/typescript/v2/UserInput.ts)
- [O5 同タグの明示スキル解決](https://github.com/openai/codex/blob/rust-v0.157.1/codex-rs/skills/src/selection.rs)
- [O6 公開App Server資料](https://developers.openai.com/codex/app-server/)
- [O7 同タグLICENSE](https://github.com/openai/codex/blob/rust-v0.157.1/LICENSE)
