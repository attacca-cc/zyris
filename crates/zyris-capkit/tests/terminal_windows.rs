#![cfg(all(windows, feature = "terminal"))]

//! The Windows half of `terminal`, which until now nothing ran.
//!
//! CI is `ubuntu-latest`, so every claim this crate makes about Windows has been carried by its
//! comments alone. One of those claims is load-bearing: `Terminal::exec`'s own doc tells a caller
//! that the way to run a PowerShell script without fighting quoting is
//!
//! ```text
//! argv: ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", "-"], stdin: <script>
//! ```
//!
//! and says in as many words that this needs "no escaping, no base64 encoding". That sentence is
//! the tool description an agent reads. If it is wrong, the agent is being told to do something
//! that does not work — which is the shape of the report these tests exist to settle: agents
//! reaching for `-EncodedCommand` because quoting bit them.
//!
//! So each test here asserts a promise the documentation already makes. A failure is not a Windows
//! quirk to work around; it is the doc being wrong, and the fix belongs in the doc and in whatever
//! the doc should have said instead.

use std::time::Duration;

use zyris::{Node, NodeKind};
use zyris_caps::{ExecOutput, Terminal, TerminalClient, TerminalServer};
use zyris_capkit::PtyTerminal;

async fn connect(term: PtyTerminal) -> TerminalClient {
    let server = Node::builder()
        .name("term-node")
        .kind(NodeKind::Service)
        .capability(TerminalServer(term))
        .build()
        .unwrap();
    let client_node = Node::builder().name("client").kind(NodeKind::Cli).build().unwrap();
    let (conn, _server) = zyris::testing::duplex(&client_node, &server).await.unwrap();
    conn.wait_capability(Duration::from_secs(2)).await.unwrap()
}

fn powershell(script: &str) -> (Option<Vec<String>>, Option<String>) {
    let argv = ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", "-"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    (Some(argv), Some(script.to_string()))
}

/// The documented pattern, doing the one thing it is documented for: carrying a script that is
/// full of the characters a command line would mangle.
///
/// Both quote kinds, a `$`, a backtick and a `%` — every one of them a character that means
/// something to some layer between here and the interpreter.
///
/// **The backtick is doubled on purpose, and the first version of this test got it wrong.** Inside
/// a PowerShell double-quoted string the backtick is *PowerShell's* escape character, so a lone
/// `` ` `` before a `b` is the escape for backspace and the script asked for one. It duly arrived:
/// the assertion failed with `\u{8}acktick`, which is not a transport mangling anything — it is
/// the interpreter doing exactly what its own quoting rules say. Doubling it is how PowerShell
/// spells a literal backtick, and that is what this asserts survives.
///
/// The distinction is the whole point of the file. This suite exists to catch the transport
/// altering a script; a test that trips over the interpreter's own syntax reports a fault in the
/// wrong place, and would have had someone rewriting `Terminal::exec`'s documentation over a
/// mistake in the example.
#[tokio::test]
async fn the_documented_powershell_pattern_carries_a_script_with_quotes_in_it() {
    let term = connect(PtyTerminal::default()).await;
    let (argv, stdin) = powershell(
        r#"$x = 'single'
Write-Output "double $x and ``backtick`` and 100% and 'nested'"
"#,
    );

    let out: ExecOutput =
        term.exec(None, argv, None, Some(20000), stdin, None, None).await.unwrap();

    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("double single and `backtick` and 100% and 'nested'"),
        "the script did not survive the trip. stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

/// A multi-line script, because a blank line between statements is the classic way `-Command -`
/// is said to stop early. If it does, the doc's "put the script on stdin" advice is only good for
/// one-liners and has to say so.
#[tokio::test]
async fn the_documented_pattern_runs_every_line_including_past_a_blank_one() {
    let term = connect(PtyTerminal::default()).await;
    let (argv, stdin) = powershell(
        r#"Write-Output "first"

Write-Output "second"
"#,
    );

    let out: ExecOutput =
        term.exec(None, argv, None, Some(20000), stdin, None, None).await.unwrap();

    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("first"), "stdout: {:?}", out.stdout);
    assert!(
        out.stdout.contains("second"),
        "the blank line ended the script early — stdout: {:?}",
        out.stdout
    );
}

/// The short form the same doc offers for one-liners: the script as an `argv` element rather than
/// on stdin.
///
/// This is where Windows is genuinely treacherous. There is no `argv` on Windows — there is one
/// command-line string — so Rust re-quotes each element by the MSVCRT rules when it builds it, and
/// PowerShell does not parse its command line by those rules. An element containing quotes is
/// exactly where the two disagree, so this asserts what the doc implies: that a quoted string
/// reaches PowerShell as written.
#[tokio::test]
async fn a_quoted_one_liner_reaches_powershell_intact_through_argv() {
    let term = connect(PtyTerminal::default()).await;
    let argv: Vec<String> = ["powershell.exe", "-NoProfile", "-Command", r#"Write-Output 'a "b" c'"#]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let out: ExecOutput =
        term.exec(None, Some(argv), None, Some(20000), None, None, None).await.unwrap();

    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains(r#"a "b" c"#),
        "the quoting was rewritten between here and PowerShell. stdout: {:?} stderr: {:?}",
        out.stdout,
        out.stderr
    );
}

/// `command` mode on Windows is `cmd /C`, which the implementation comment calls the worst quoting
/// rules of the three. Worth having something on it either way: it is what a caller gets by
/// reaching for the obvious field.
#[tokio::test]
async fn command_mode_runs_through_cmd() {
    let term = connect(PtyTerminal::default()).await;

    let out: ExecOutput = term
        .exec(Some("echo hello-zyris".into()), None, None, Some(20000), None, None, None)
        .await
        .unwrap();

    assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("hello-zyris"), "stdout: {:?}", out.stdout);
}

/// `exec` bounds the whole run and reports it, on this platform too — the timeout path kills a
/// process tree, and how that is done is one of the places Windows and Unix differ most.
#[tokio::test]
async fn a_timeout_is_reported_rather_than_hanging() {
    let term = connect(PtyTerminal::default()).await;
    let (argv, stdin) = powershell("Start-Sleep -Seconds 30\n");

    let out: ExecOutput =
        term.exec(None, argv, None, Some(2000), stdin, None, None).await.unwrap();

    assert!(out.timed_out, "a 30s sleep under a 2s budget should time out: {out:?}");
    assert_eq!(out.exit_code, -1);
}
