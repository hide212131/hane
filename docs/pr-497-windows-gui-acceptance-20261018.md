# PR #497 Windows GUI受入記録（2026-10-18）

## 対象

- 確認対象: PR #497、Windows ARM64の通常ビルド
- 基準版: `main` `aab1dd5a28dc1383d34e09e0e2f01b634ad6301a`
- PR head: `d2ebf5cfa98d9d35e470dde5f9d24cc20887da5f`
- 試験した製品コード: `a043ba76c9337eecbf1b78a88f8db8bac7cb502e`
- 基準版バイナリSHA-256: `E5CBACE2A560A1D27312C4FBDECD556D0D663DF44B63D92E49ECB94E9D9613EC`
- 候補版バイナリSHA-256: `F55EB569DDBC6EC94F4C6D9BF96F1BB657CE430BFCD16A1236053749F27361DA`

これは実装仕様§10のG01〜G06について、このWindows環境で観測できた範囲の記録である。

## 結果

| ID | 結果 | 観測 |
| --- | --- | --- |
| G01 | PARTIAL | Filterで日本語IMEの未確定文字と候補一覧を確認。Escapeで選択文字列とフォーカスが戻り、変換後のEnterでもフォーカスが維持され本文は変化しなかった。規定されたEnter/Escapeの全段階は完了していない。 |
| G02 | PARTIAL | `.md` / `.MD` のrename、取消、衝突時の失敗、再試行、再読込を確認。rename欄でのIME合成は未確認。 |
| G03 | PASS | Filterとrenameの両欄で `A😀éZ`、カーソル移動、Shift選択、Delete/Backspace、Copy/Cut/Paste、取消を確認し、他方の欄への表示漏れは観測しなかった。 |
| G04 | PARTIAL | Filter候補の先頭・中間・末尾、逆方向選択、sidebar resize後、commit後のクリック位置を確認。renameは先頭・末尾を確認したが、中間位置とrename行の移動は未完了。 |
| G05 | PASS | Filter、rename、本文、content search、document find、AI selection popup、settings routesでフォーカス分離を確認。AIは未設定で、リクエストは送信していない。 |
| G06 | NOT RUN | 基準版と候補版の両方に対する対称な5秒遅延ビルドとGUI比較を実施していない。当時、Computer Useのアプリ一覧にネイティブHaneウィンドウがなく、比較操作を開始できなかった。 |

## 判定と証拠

- G01/G02/G04は部分確認、G06は未実施のため、G01〜G06全体の受入完了とはしない。PR #497とIssue #305の受入状態はそれぞれの記録に従う。
- G01〜G05の画面キャプチャ、通常ビルド、ビルドログはローカルの検証記録に保持した。バイナリやキャプチャはこの文書PRへ含めていない。
- KeyhacのHookは試験開始時ONで、終了時も状態のreadbackによりONへの復帰を確認した。
- 後日のChatGPT更新後にネイティブアプリ一覧の取得は復旧したが、Haneは再起動しておらず、G04/G06の再試験も行っていない。この後日の確認をG06の証拠には数えない。
