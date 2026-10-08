//! **Lite is a config file, and this is what holds it.**
//!
//! `crates/zyris-app/tauri.lite.conf.json` is the whole of the axis on the Tauri side: Tauri
//! creates every window a config declares before `setup` runs and none that it does not, so an
//! `app.windows` that is *present and empty* is what makes the binary a tray with nothing behind
//! it. The other half — no speech — is the absence of `voice` on the command line, which no test
//! can state about a file and which `the_app_never_asks_whether_voice_is_compiled_in.rs` keeps
//! honest from the other end: the app behaves identically either way, so a build with the feature
//! off is not a second program.
//!
//! **Two ways of writing it wrong are silent.** Leaving `app.windows` out of the Lite config
//! merges the ordinary config's window back in, because `--config` is a merge and not a
//! replacement; and a Lite build that shares the ordinary build's `productName` or `identifier`
//! is not a second installation at all — it is the first one, replaced. Neither fails a build,
//! and a Lite installer that opens a window is an installer nobody can tell from the ordinary
//! one. So the file is read, the way `announce.rs` reads the README.
//!
//! The update file is the third: `latest.json` describes the build *with* speech, so a Lite copy
//! reading it would be offered an update that puts the window and the microphone back.

use serde_json::Value;

/// The Lite build's config, merged over the ordinary one by `--config` on the command line.
const LITE: &str = "tauri.lite.conf.json";

/// The ordinary build's, which is also what `tauri build` reads with no `--config`.
const ORDINARY: &str = "tauri.conf.json";

fn config(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is a JSON document: {error}", path.display()))
}

#[test]
fn the_lite_config_declares_no_window_at_all() {
    let lite = config(LITE);

    // **`expect` rather than a default.** An absent `app.windows` is not an empty one: the file is
    // merged into `tauri.conf.json`, which declares a window, so leaving the key out inherits it.
    let windows = lite.pointer("/app/windows").unwrap_or_else(|| {
        panic!(
            "{LITE} has no `app.windows`. The file is merged *into* {ORDINARY}, so leaving the \
             key out keeps that file's window and the Lite installer opens one."
        )
    });

    assert_eq!(
        windows.as_array().map(Vec::len),
        Some(0),
        "`app.windows` in {LITE} has to be an empty array, and is {windows}"
    );
}

#[test]
fn the_ordinary_config_still_declares_its_window() {
    // The other half of the same rule: a Lite config that emptied a list already empty would
    // prove nothing, and the ordinary build is what every existing install is running.
    let ordinary = config(ORDINARY);

    let windows = ordinary
        .pointer("/app/windows")
        .and_then(Value::as_array)
        .expect("the ordinary build declares its windows in tauri.conf.json");

    assert_eq!(windows.len(), 1, "the ordinary app is one window");
    assert_eq!(
        windows[0]["label"], "main",
        "and everything that reaches it — the tray, the single-instance callback, the bridge — \
         addresses it by that label"
    );
}

#[test]
fn the_lite_build_is_its_own_installation() {
    let lite = config(LITE);
    let ordinary = config(ORDINARY);

    let name = lite["productName"]
        .as_str()
        .expect("the Lite build names itself");
    assert_ne!(
        name,
        ordinary["productName"]
            .as_str()
            .expect("the ordinary build names itself"),
        "two installers that name themselves the same thing are one installer: the NSIS install \
         directory, the Start Menu entry, the `.deb` and `.rpm` package names and the macOS bundle \
         all come from this string, and the release would attach two assets under one name"
    );

    let identifier = lite["identifier"]
        .as_str()
        .expect("the Lite build has an identifier");
    assert_ne!(
        identifier,
        ordinary["identifier"]
            .as_str()
            .expect("the ordinary build has one"),
        "the identifier is the Windows uninstall entry, the webview's data directory and what \
         `tauri-plugin-single-instance` keys on; sharing it makes installing one of these remove \
         the other"
    );
    assert!(
        identifier.starts_with("cc.attacca.zyris."),
        "{identifier} is in this project's reverse-DNS namespace, so that the two cannot collide \
         with another application's"
    );
}

#[test]
fn the_lite_build_reads_the_update_file_written_for_it() {
    let lite = config(LITE);

    let endpoint = lite
        .pointer("/plugins/updater/endpoints/0")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!(
                "{LITE} does not name an updater endpoint. It would inherit {ORDINARY}'s, which \
                 describes the build *with* speech — an installed Lite copy would be offered an \
                 update that puts the window and the microphone back."
            )
        });

    assert!(
        endpoint.ends_with("latest-lite.json"),
        "the Lite build reads {endpoint}, which is not the file written for it: `.github/\
         workflows/release.yml` writes `latest-lite.json` beside `latest.json` from the \
         `Zyris-Lite` assets, and both are attached to the same release"
    );
}
