#[test]
fn offline_acceptance_suite() {
    let result = std::process::Command::new("python3")
        .arg("tests/test_cli.py")
        .env("RECONCILE_BIN", env!("CARGO_BIN_EXE_payment-reconcile"))
        .status()
        .expect("Python 3 standard library is required for offline RPC tests");
    assert!(result.success(), "offline acceptance tests failed");
}
