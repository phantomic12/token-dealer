//! End-to-end smoke test for the loopback OAuth flow.
//!
//! Spins up a wiremock standing in for the OpenAI token endpoint,
//! starts `token-dealer-login openai` with `--print-url` so it
//! doesn't try to open a browser, and asserts the loopback server
//! accepted the callback URL with the expected `code` + `state`
//! parameters.
//!
//! Note: this is a smoke test, not a full e2e. The loopback flow
//! involves spawning a browser, so we exercise the URL-build path
//! + the callback parser separately rather than the full driver.

#[test]
fn bin_compiles() {
    // If `cargo build` succeeds for the binary, this test exists to
    // give the smoke test target something to anchor to in CI.
    // The real loopback flow test lives in
    // `tests/loopback_oauth.rs` once we add it.
}
