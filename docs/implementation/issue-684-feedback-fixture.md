# Issue #684 隔離実経路fixture

このDraft PRは、Issue #684のdefault-branch採用後の通常Claude経路を一度だけ確認するための一時fixtureです。製品機能・ユーザーデータ・製品仕様は変更しません。製品へmergeせず、実証と記録の後に閉じます。

## 固定した境界

- base main: `26fd1e1af55d2f8b43efd97c48fbb41e2e73f337`
- H0は初回Claude handoff直前のPR current headとして記録し、H1/H2はそれぞれtrusted finalizerのpush後headとします。
- 初回Claude workerが追加できるパスは `crates/session/tests/aadw_feedback_fixture.rs` だけです。
- 初回workerの追加内容は、次の小さい統合テストだけです。初回の期待値 `5` は意図的に誤り、期待する実値は `4` です。

```rust
#[test]
fn aadw_feedback_fixture_expected_value() {
    let observed = std::hint::black_box(4_u32);
    assert_eq!(observed, 5, "Issue #684 feedback fixture: expected value is intentionally wrong");
}
```

- 検証失敗後、追加依頼のworkerが変更できるのも同じ1ファイルだけです。正解は期待値を `4` にすることです。assertを削除・弱化・ignore化したり、失敗条件を避けたりしてはいけません。
- workflow、認証、権限、製品コード、他PRのファイルは対象外です。
- 初回実装・追加修正のどちらも通常の `@claude` / `AADW_COMMANDER_HANDOFF_V3` 経路を使います。workerはRead/Edit/Write/Glob/Grepだけを使い、検証・pushはtrusted workflowが行います。

## 成功時に記録する観測

H0/H1/H2、各run/attempt/job、実行model、source/追加handoff、artifactのrun-bound metadata、test/lint outcomeと実終了コード、失敗API名・安全なHTTP status、workerがfeedbackを読んだ根拠、stale head・出力欠落・job障害・保護パス拒否の実接続結果を記録します。fixture自体の成功はIssue #684の代表的な実修正や速度効果の証明には使いません。
