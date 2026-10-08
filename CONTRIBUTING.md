# Contributing to Zyris

Thank you for helping. Zyris gives a remote agent a shell, your files, your screen and your
keyboard, so changes here are held to a careful standard: every behaviour should be explained,
tested, and checked on the platform it affects.

## Before you start

- **Small fixes** (a typo, an obvious bug with a one-line cause) can go straight to a pull request.
- **Anything larger** — a new capability, a new dependency, a change to how the app connects,
  stores secrets or ships — starts as an issue, so the approach is agreed before the code is
  written.
- **Security problems are not filed as public issues.** Report them privately through GitHub's
  *Report a vulnerability* button on the repository's Security tab.

## Setting up

### With Nix (recommended)

The flake carries a development shell with the toolchain and every system library the build
needs, speech included:

```bash
nix develop        # or, with direnv: `direnv allow` once, and the shell loads on `cd`
```

### Without Nix

Install a Rust toolchain, Node 24 and pnpm 11, and the system libraries listed under
[Building](README.md#building) in the README. That list is the one CI installs.

### First build

The frontend has to exist before the Rust crates compile, because `tauri.conf.json` embeds
`ui/dist`, which is not committed:

```bash
pnpm install
pnpm --filter zyris-ui build
pnpm tauri dev          # the app, with the dev server and hot reload
```

## Checking your change

Run what CI runs before you open a pull request:

```bash
pnpm --filter zyris-ui test
cargo test --workspace
```

Format Rust with `cargo fmt` before committing.

**Speech is behind the `voice` feature** and is not part of `cargo test --workspace`. If you touch
`crates/zyris-voice`, also run:

```bash
cargo test -p zyris-voice --features voice
```

The tests that need real models are skipped unless you point them at the model files:
`ZYRIS_WHISPER_MODEL` for a whisper `.bin`, `ZYRIS_TTS_MODELS` for the Supertonic directory, and
`ZYRIS_ONNX_WHISPER` for an exported ONNX whisper directory (`onnx-community/whisper-base`; the
revision and file list are at the top of `tests/onnx_whisper_hears_what_whisper_cpp_hears.rs`).
Run them with `--release`; a debug whisper takes minutes per sentence.

**Say how you checked a change that no test covers.** A microphone, a push-to-talk key, a tray, a
phone and an installer can only be checked by hand. Name the platform and what you saw in the pull
request.

### Workspace rules the tests enforce

- **No manifest may turn on `zyris-voice`'s `voice` feature.** Cargo unifies features, so one
  mention would put the whole audio stack into every `cargo test --workspace`. Forward it from a
  feature, as `zyris-app`'s `voice` does. `nothing_turns_the_feature_on_by_itself.rs` checks this.
- **`zyris-app` never asks `cfg!(feature = "voice")`.** The on and off builds run the same code;
  `the_app_never_asks_whether_voice_is_compiled_in.rs` checks this.
- **whisper.cpp is built for the AVX2 set** (`.cargo/config.toml`), never for the CPU that happens
  to build it. A release without those settings refuses to build — and **every path that calls
  into it is behind a run-time AVX2 check** (`zyris-voice`'s `cpu` module, asked once in
  `zyris_voice::start`). A machine without AVX2 gets a Voice screen that says so, never a
  `SIGILL` before its first window: that compiled code carries no check of its own, so the check
  has to be ours.

### Phones and macOS

Android and iOS builds are generated rather than committed (`crates/zyris-app/gen/` is ignored).
`.github/workflows/mobile.yml` is the reference for what a phone build needs, and it runs on every
pull request that touches the app, the runtime, the voice crate, the UI or the lockfile. A macOS
build with speech (`voice,metal`) runs on every pull request that changes code.

A change to documentation alone (`*.md`, `docs/`, `LICENSE`) skips the builds.

## Writing code

- **Match the code around you.** Naming, error handling, and how much a comment says.
- **Comments explain why, not what.** Measurements, platform quirks and rejected alternatives
  belong beside the line they justify, with a date when they were measured.
- **Keep the smallest change that is correct.** No abstraction with one user, no configuration
  for a value that never changes, and no new dependency for what a few lines can do.
- **Fix causes, not symptoms.** If a bug is in a function several callers share, fix it there.
- **English everywhere written down:** code, comments, identifiers, log messages, commits,
  pull requests and documentation. UI text is English too, unless the change is a translation.

## Commits

Use `type(scope): summary`, in the present tense, saying what the change does for the user or the
project. The scope is optional:

```text
feat(android): speech on the phone
fix(voice): GPU transcription on Linux and macOS, and no crash on quit
build(nix): package 0.1.2, which links the Vulkan loader
ci(mobile): phone builds on tags and by hand only
```

Types: `feat`, `fix`, `build`, `ci`, `test`, `docs`, `refactor`. Explain in the body what was
wrong and how you know the change fixes it. Keep commits focused: one concern each.

## Pull requests

- Branch from `main` with a name like `feat/…`, `fix/…`, `build/…` or `docs/…`, and target `main`.
- Describe what changes and why, and how you verified it.
- Link the issue it resolves, with `Closes #N` when it closes one.
- **CI must be green** before a merge: the tests on Ubuntu and Windows, the macOS build, and
  the phone builds when they run. A change to the release workflow should also pass a manual
  run of it on the branch.
- A maintainer reviews and merges. Keep the history readable; fix-up commits are fine while a
  review is in progress.

## Releases

Maintainers cut releases. The version lives in two places, which must agree: `Cargo.toml`
(`[workspace.package]`) and `crates/zyris-app/tauri.conf.json`. Pushing a `v*` tag then:

- builds Windows, macOS and Linux (`release.yml`), and Android and iOS (`mobile.yml`);
- signs the update files and writes `latest.json`, which installed copies read to update
  themselves.

After a release, `nix/package.nix` is bumped to the new `.deb` in a follow-up pull request.

## License

Zyris is licensed under the [Apache License 2.0](LICENSE). By contributing, you agree that your
contribution is licensed under the same terms, as section 5 of the license describes. No separate
agreement is needed.
