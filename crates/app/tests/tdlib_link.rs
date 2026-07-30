//! Proves `tdlib-rs` links and `libtdjson.dylib` resolves at runtime (architecture.md §9.2).
//!
//! `tdlib_rs::create_client()` is a synchronous FFI call straight into `td_create_client_id`
//! in `libtdjson`. If linking failed or the dylib didn't resolve via `@rpath`, dyld aborts
//! the process before this test body ever runs; reaching the assertion is itself most of the
//! proof, and the returned id gives something concrete to assert on.

#[test]
fn tdlib_executes_synchronously() {
    let client_id = tdlib_rs::create_client();
    assert!(
        client_id > 0,
        "expected a positive TDLib client id, got {client_id}"
    );
}
