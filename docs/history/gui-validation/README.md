# GUI検証の実証記録

実証の対象SHA・手順・結果を区別して追跡するための索引。

| 実証 | 記録 |
| --- | --- |
| GitHub-hosted macOS上での起動、対象ウィンドウ撮影、終了 | [run 34254547035](https://github.com/hide212131/hane/actions/runs/34254547035) |
| ASCII入力・保存・undo/redo・再オープン、日本語IME確定・保存、OSホイール | [run 34259421224](https://github.com/hide212131/hane/actions/runs/34259421224) |

後者の対象は Hane `866cfbc0cdcca8021aff0439b53e7c247fc7e54c`、手順は `4cea7bde635203fa2bf85823ffc940e88aceea71`。これらは指定操作のsmoke testであり、すべてのGUI操作が検証済みという意味ではない。新しいSHAや実行世代の判定に、古い実行を流用しない。
