#[test]
fn aadw_feedback_fixture_expected_value() {
    let observed = std::hint::black_box(4_u32);
    eprintln!("\\x1b[31mIssue #684 ANSI fixture diagnostic\\x1b[0m");
    assert_eq!(observed, 5, "Issue #684 feedback fixture: expected value is intentionally wrong");
}
