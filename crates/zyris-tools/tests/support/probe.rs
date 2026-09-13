//! Where cargo leaves this crate's copy of `zyris-mcp`'s probe server.
//!
//! Declared as an example in this package's manifest, pointing at the one source file in
//! `zyris-mcp`, rather than copied: a second copy of a foreign MCP server is a second thing to
//! keep in step with the protocol, and the whole value of the probe is that it is not written by
//! the code under test.
//!
//! Shared by every integration test here that needs a real MCP server, through
//! `#[path = "support/probe.rs"] mod probe;`. A directory under `tests/` with no `main.rs` is not
//! a test target of its own, so this compiles into whichever test includes it and never runs
//! alone.

use std::path::PathBuf;

/// The probe server's path, or a panic saying how to get one built.
///
/// There is no `CARGO_BIN_EXE_` for an example, so this walks out of the test binary's own
/// directory the way cargo lays `target/<profile>/` out.
pub fn probe_server() -> String {
    let mut directory = std::env::current_exe().expect("the test binary knows its own path");
    directory.pop();
    if directory.ends_with("deps") {
        directory.pop();
    }
    let path: PathBuf = directory
        .join("examples")
        .join(format!("mcp_probe_server{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "the `mcp_probe_server` example is not at {}. `cargo test` builds examples; a narrower \
         selection such as `cargo test --test servers_come_and_go` does not. Run \
         `cargo test -p zyris-tools`.",
        path.display()
    );
    path.into_os_string().into_string().expect("a path cargo produced is UTF-8")
}
