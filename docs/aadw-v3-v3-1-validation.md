# AADW v3 V3-1 検証記録

Issue #333 の V3-1 について、既存の AADW v2 Claude 経路を ChatGPT アプリ内 Commander から実際に起動し、受理 run、worker patch、trusted finalizer、push 後の current-head CI を対応付けるための検証用ファイルです。

この初期コミットは PR を作るための最小の候補だけを置きます。現時点では Claude 実行、patch、finalizer、CI の成功を主張しません。

## Claude worker に依頼する変更

Claude worker は、この節の直後に `worker-edit` という見出しを一つ追加し、次を短く記録してください。

- この変更が Issue #333 / V3-1 の経路確認用であること。
- worker 自身は shell、test、git、push を実行していないこと。
- CI、review、GUI、merge の成功は worker の編集結果だけでは主張しないこと。

ほかのファイルは変更しないでください。
