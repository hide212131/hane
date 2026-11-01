# agent-desktopによるmacOS / Windows GUI検証

Haneのfocused scenarioをローカルの`agent-desktop` CLIで操作・観測する手順。既存のComputer Use、Hosted GUI Validation、[ローカルCLI](local-gui-validate-cli.md)と併用できる。判定は[Commander Policy](aadw-command-policy.md)、障害時は[診断・復旧手順](gui-test-recovery.md)に従う。新しいrunnerや状態管理は追加しない。

## 導入と開始前確認

macOS / WindowsともNode.js 18以上の環境で、今回確認した版を明示して導入する。

```sh
npm install -g agent-desktop@1.0.2
agent-desktop version
agent-desktop skills get desktop
agent-desktop skills get platform
agent-desktop status
agent-desktop permissions
```

初回操作前に同梱skillを読む。各コマンドの引数は`agent-desktop <command> --help`で確認する。同梱skillはインストール済みbinaryと対応する。更新時にはversionとskillを取り直す。シェルから見つからない場合はNode/npmのユーザー用binがPATHに入っているか確認する。

- macOSは13以上。呼出元アプリのアクセシビリティ権限が必要。画像取得には画面収録も必要。欠ける場合だけ`agent-desktop permissions --guide`で案内し、利用者がOSの認証と許可を行った後に再確認する。Automationが`unknown`であるだけで全操作不能とはしない。
- Windowsは10 1809以上 / Server 2019以上、x64またはARM64のログイン済み対話セッションで実行する。Windows側の接続済みChatGPTアプリから直接操作する。MacからVM画面を操作しない。サービスのSession 0、ロック画面、UACのsecure desktopは対象外。通常権限Haneと同じintegrity levelで開始し、UIPI拒否を無条件の管理者実行やUAC操作で回避しない。
- GUIの占有を確認する。headed入力中は利用者の操作、別テスト、デスクトップ/Spaces切替と競合するため、短い操作区間では同じデスクトップを保つ。切替後は前面状態・window・bounds・refを取得し直す。切替だけを製品不具合やテスト基盤故障の根拠にしない。

## Haneの準備

1. 対象のcurrent head、必要なbase / merge context、実際のビルド元SHAを記録する。導入確認に既存の配布版を使う場合はversion・binary hashとSHA不明を明記し、PR候補の受入evidenceに流用しない。
2. 一意の一時workspaceとfixtureを作り、専用の`HANE_STATE_DIR`を設定して起動する。通常の文書・stateを使わない。実PID、実行ファイルの絶対パス、起動引数、state、fixture、所有チャットを記録する。
3. macOSでbundleが必要なら[既存harness](../.agents/skills/hane-gui-test/references/gpui-macos-harness.md)に従い一意のbundle IDを用意する。Windowsでも実際のexeを特定する。起動しただけで入力可能としない。
4. `list-apps`、`list-windows --app <実名>`から対象PIDとwindow IDを照合する。以下の名前、ID、ref、座標、画像パスは例であり、そのまま再利用しない。

## 観測 → 操作 → 再観測

```sh
agent-desktop list-apps --app HaneGuiTest
agent-desktop list-windows --app HaneGuiTest
agent-desktop snapshot --app HaneGuiTest --include-bounds
agent-desktop screenshot --window-id w-123 /absolute/test-root/before.png
```

HaneのGPUI本文は、操作可能なAX/UIA text fieldとして公開されているとは限らない。取得できたボタン・タブ・native dialogはrefで操作し、独自描画の本文は画像と許可された物理入力で確認する。`set-value`やモデル直接変更を実編集操作の代用にしない。

```sh
# 直前のsnapshotから選んだ実refを使う
agent-desktop click '@s8f3k2p9:e3'
agent-desktop snapshot --app HaneGuiTest --wait-for 'button:編集モードに切り替え' --wait-timeout 5000
```

`@<snapshot_id>:e<N>`の完全なrefを使用する。PowerShellでも必ず引用符で囲む。UI変更後はfresh snapshotを取り、`STALE_REF` / `AMBIGUOUS_TARGET`なら対象を絞り直す。sessionを使う場合は全コマンドに同じ`--session`または`AGENT_DESKTOP_SESSION`を指定する。

### 物理入力

利用者が依頼したGUIシナリオの範囲で`--headed`を使う。headless失敗だけを根拠に物理操作の許可を拡張しない。座標は直前の画像・bounds・display scaleから実画面に対応付ける。ウインドウ画像には影や余白が含まれ得るため、画像pixelをそのまま画面座標にしない。座標入力自身にはwindow identityや前面化の保証がない。

```sh
agent-desktop --headed focus-window --window-id w-123
agent-desktop snapshot --window-id w-123 --include-bounds
agent-desktop --headed mouse-click --xy 600,300
# macOS
agent-desktop --headed press cmd+s --window-id w-123
```

Windows側のPowerShellでは次の形式を使う。

```powershell
agent-desktop skills get desktop
agent-desktop skills get platform
agent-desktop list-apps --app hane
agent-desktop list-windows --app hane
agent-desktop snapshot --app hane --include-bounds
agent-desktop click '@s8f3k2p9:e3'
agent-desktop --headed press ctrl+s --window-id w-123
agent-desktop screenshot --window-id w-123 'C:\test-root\after.png'
```

Windowsの`press --app`はforeground queueを使うため、背面の別アプリを狙ったつもりで送らない。必要な前面状態と対象windowを確認する。`type`はrefが必要で、Windowsでは`--headed`も必要。本文にtext refがなければ`press`や観測済み座標などの対応経路を選ぶ。

コマンドの`ok: true`や`delivered_unverified`は期待結果の証明ではない。操作後のsnapshot、画像、保存ファイルのbytes等で確認する。非同期の変化は`wait`または`--wait-for`で待ち、古い画像や直後の一枚だけで判定しない。JSONの`data.disposition` / `error.disposition`を読み、同じ操作を直接再送するのは`retry: safe`の場合だけとする。`unsafe` / `unknown`ならlive stateを観測し、二重入力・二重toggleを避ける。

## 最小smokeとevidence

- 対象windowの画像とAX/UIA snapshotを取得する。
- fixtureだけを使い、AXボタン操作と座標クリックをそれぞれ実施して状態変化を観測する。
- 本文へのキー入力・保存を行い、画面と実ファイルの変更を照合する。Undo/Redoや再起動は対象scenarioで必要な場合に加える。
- IMEの未確定・候補・確定、Finder/Explorerからの外部drop、native dialogは個別に実施する。Unicode文字列の挿入をIME変換の証拠にせず、synthetic dragの送信成功をHaneのdrop受信の証拠にしない。
- version、OS、対象SHA / base context、PID・path・state、操作前後と各結果、未実施条件、後片付けを記録する。画像・snapshot・traceは私的なデスクトップ情報を含み得るのでローカルの隔離領域に置き、公開する前に内容を確認する。全画面画像やclipboardを無条件に公開しない。
- 終了前に所有PID/pathを再照合し、専用Haneだけを通常終了する。終了確認後にbundle/stateを片付ける。`pkill Hane`や名前による一括停止は使わない。証拠を残す場合は保存先を報告する。

## 障害時

`WINDOW_NOT_FOUND`、`window_without_accessibility_element`、黒い画像、座標操作の無効果を同じ原因と決めつけない。権限・PID生存・desktop・最小化・window ID・前面・AX・画像・入力を分け、[復旧手順](gui-test-recovery.md)に沿って一つずつ確認する。現在の同梱troubleshootingは次から読める。

```sh
agent-desktop skills get platform troubleshooting
```

platformのreference名が異なる場合は`agent-desktop skills list`と同梱platform skillで確認する。macOSは`agent-desktop skills get macos troubleshooting`、Windowsは`agent-desktop skills get windows troubleshooting`で取得できる。macOSの専用bundleを通常の「開く」で再アクティブ化する場合も、同じPID・同じfixtureであることを確認し、windowが現れるまで待ってfresh snapshotから再開する。別アプリで比較する際は無害な専用fixtureだけを操作する。

復旧後は軽量な入力確認に続けて元scenarioを再実行する。agent-desktopで必要な条件を観測できなければ既存の許可されたComputer Use / Hosted経路を使い、未実施・unknownをpassにしない。MacのCLIをWindows GUI evidenceへ流用しない。

## 導入時の観測（2026-11-01）

このMacのApple Silicon環境でnpm版1.0.2を導入。Accessibility / Screen Recordingは`granted`。配布版Hane 0.31.1のコピーを一意bundle・専用state・fixtureで起動した。ビルド元SHAは不明で、特定PRの受入検証ではない。

初回にはwindow画像取得エラー、window/AX参照喪失、効果を観測できない座標入力があった。利用者からデスクトップ切替の申告があり、通常の再アクティブ化とfresh snapshotの後、切替を控えた再実行ではAXボタン操作と座標クリックによるモード切替、数字キー入力・保存・実ファイルの変更、window画像取得を確認した。初回失敗の原因を切替だけに断定しない。

再実行で保存したfixtureは`Agent Desktop smoke\n\n1`から`Agent Desktop smoke\n\n1\n2`へ変化した。Windows実機、IME変換、外部drop、Undo/Redoは今回の確認範囲に含めていない。Windowsの導入・操作例は同梱Windows skillを基にした手順であり、実機合格の記録ではない。

出典: [公式repository](https://github.com/vercel-labs/agent-desktop)、[導入説明](https://github.com/vercel-labs/agent-desktop/blob/main/docs/install.md)、インストール済み1.0.2の`skills get desktop` / `skills get platform` / `skills get windows`。
