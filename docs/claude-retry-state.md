# Claude retry state（AADW v1 履歴）

> [!IMPORTANT]
> この文書は AADW v1 の retry state / reconcile 契約を記録した履歴資料であり、現行の運用契約ではない。

現行運用は Claude の失敗から persistent な retry state を導出しない。provider / infrastructure failure と product failure を区別し、current facts / evidence と Commander Policy からChatGPT Commanderが次の一つのactionを判断する。

現行の判断は [AADW Commander Policy](aadw-command-policy.md#7-provider--infrastructure-failure) を正とする。v1 の全文は [`docs/history/aadw-v1/claude-retry-state.md`](history/aadw-v1/claude-retry-state.md) に保存する。
