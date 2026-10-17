# Issue #510 U0: アプリ本体更新コアの実装準備

状態: Claude実装待ち。これは準備commitであり、製品コード・テストはまだ変更していない。

- 対象Issue: [#510](https://github.com/hide212131/hane/issues/510)
- 基準main: `777689ae02d78c90aabbd1af1a9619aea6f89035`
- 採用済み設計: [ADR-0034](../adr/0034-zip-application-updates.md)、[実装仕様](../application-update-implementation-spec.md)、[引き継ぎ](../application-update-implementation-handoff.md)
- 設計PR: [#509](https://github.com/hide212131/hane/pull/509)

## 今回の範囲

U0として、後続UI・OS adapter・配布変更が依存できる、ネットワーク取得物を安全に候補化する共通Rust coreを実装する。

- 新規`crates/update` (`hane-update`) の型付きmodel、SemVer/target/release asset選択、schema 1 catalogとraw bytes署名検証、期限・sequence・protocol・hash検証。
- `crates/update` のDownload/staging境界とGitHub endpoint・redirect許可、応答/総byte上限を守る処理。決定的に失敗を試験できるfake transport/clock/filesystem seamを含める。
- ZIP全体を展開前に検査し、path traversal、衝突、link、予約名、暗号化/非対応compression、宣言と実byte上限超過を拒否するarchive検証。
- InstallId、署名済みinventory、旧/新候補の固定identity、`ApplyPlan`/`UpdateJournal`の型とdurable更新/回復判定の共通部品。OS固有のprocess lock、file replacementと実電断試験は後続U4で行う。
- 既存workspaceへcrateを統合し、直接関連するunit/integration testsを追加する。GPUI、文書本文、AI資格情報、runtime installへの依存は持ち込まない。

## 今回の受入条件

1. catalogは署名対象のraw bytesとdomain separatorで検証され、未知key/schema、重複JSON key、改ざん、期限切れ、sequence後退/同sequence別bytes、対応外protocol/targetを拒否する。
2. release versionとasset名をSemVer/OS targetで選び、404・重複asset・非stable・不正redirect・上限超過・途中切断・cancelを「最新」と誤認せず識別する。
3. stageへ出す前にZIPのpath・entry・総量・hash/inventoryを検査し、拒否fixtureでstage外のファイルが変わらないことを示す。
4. journalは状態遷移と実disk identity/hashの照合方針を持ち、旧/新いずれかを推測で上書きしない回復判定をテストする。OS process leaseと実クラッシュ耐性を検証済みとは主張しない。
5. test鍵/fixtureのみを使い、秘密署名鍵、実Team ID、OS signing credentialを追加しない。未設定のproduction trust materialでは候補を適用可能にしない。
6. 導入crateのlicense/MSRV/featuresとCargo.lockを確認し、変更対象に対応するworkspace test/Clippyを実行して結果を報告する。

対応する設計受入表: U-01〜U-06とU-12のうち共通coreで決定的に試験できる条件。U-10のOS process lease、U-11/U-13の実Explorer/macOS署名・notarization、U-14の二版適用、U-15のGUI回帰はこの段階で完了扱いにしない。

## 対象外

- 設定画面、ユーザー向けCheck/Download/Restart UI、通常起動時のservice配線。
- Windows/macOSの実OS lock、helper handshake、配置へのファイル置換、限定health、rollback実行。
- release workflow、公開catalog署名、Apple Developer ID/notarization、秘密情報設定、実配布の有効化。
- 文書保存・AI停止・Explorer連携の変更。

このPRは`Refs #510`とし、mergeしてもIssue #510をcloseしない。後続段階は受け入れ済みAPI・current mainを確認して別PRにする。
