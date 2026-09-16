# Final judgement（AADW v1 履歴）

> [!IMPORTANT]
> この文書は AADW v1 の Copilot final judge / merge gate 契約を記録した履歴資料であり、現行 AADW v2 の契約ではない。

AADW v2 では ChatGPT が唯一の Commander として current PR head と current target branch / base context に対応する evidence を評価する。head が同じでも target branch が進んだ場合は、旧 CI / review / GUI evidence を無条件に current とみなさない。ただし意味判断だけで merge は行わず、merge 直前に expected head、current target branch / base context、current context の required CI、必要と判断した GUI validation、GitHub mergeability を再確認する。

現行の判断は [AADW Commander Policy](aadw-command-policy.md#45-merge) を正とする。v1 の全文は [`docs/history/aadw-v1/final-judgement.md`](history/aadw-v1/final-judgement.md) に保存する。
