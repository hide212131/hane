# Agentic implementation recovery（AADW v1 履歴）

> [!IMPORTANT]
> この文書は AADW v1 の `/implement` と自動回復経路を説明した履歴資料であり、現行 AADW v2 の契約ではない。

AADW v2 では provider / infrastructure failure を product failure と混同せず、必要な evidence を取得できなければ fail closed とする。quota timer、自動 multi-provider fallback、retry reconcile を初期 v2 の前提にしない。

現行の判断は [AADW Commander Policy](aadw-command-policy.md#7-provider--infrastructure-failure) を正とする。v1 の全文は [`docs/history/aadw-v1/agentic-implementation-recovery.md`](history/aadw-v1/agentic-implementation-recovery.md) に保存する。
