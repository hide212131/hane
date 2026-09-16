# Final judgement（AADW v1 履歴）

> [!IMPORTANT]
> この文書は AADW v1 の Copilot final judge / merge gate 契約を記録した履歴資料であり、現行 AADW v2 の契約ではない。

AADW v2 では ChatGPT が唯一の Commander として current PR head に結び付く evidence を評価する。target branch / base context がその evidence の主張に影響し得る場合は current base との整合も確認し、head が同じでも target branch が進んだことで前提が変わり得る旧 CI / review / GUI evidence を無条件に current とみなさない。一方、base の変更が evidence の主張に影響しないと Commander が current facts と check / scenario の性質から判断できる場合は、その根拠を GitHub 上に残して head に結び付く evidence を利用できる。ただし意味判断だけで merge は行わず、merge 直前に expected head、base-sensitive な evidence の current target branch / base context、current context の required CI、必要と判断した GUI validation、GitHub mergeability を再確認する。

現行の判断は [AADW Commander Policy](aadw-command-policy.md#45-merge) を正とする。v1 の全文は [`docs/history/aadw-v1/final-judgement.md`](history/aadw-v1/final-judgement.md) に保存する。
