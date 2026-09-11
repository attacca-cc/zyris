# Zyris

A desktop app that keeps your computer connected to [Attacca](https://attacca.cc), and lets you
talk to it.

It runs in the tray, dials Attacca over one websocket, and announces what this machine can do —
a shell, the filesystem, the keyboard and mouse, the screen, file transfer to your other
machines. Your agents call those as tools. Local MCP servers you already run are promoted the
same way, so their tools sit beside the built-in ones and an agent cannot tell the difference.

The window is for settings and for watching. Everything that matters keeps running with it
closed, including speech: hold a hotkey and talk, or turn on the wake word and say its name.
Answers come back spoken while they are still being written.

Built on the [Zyris protocol](https://github.com/attacca-cc/zyris-protocol) with Rust and Tauri.
Installs as an `.exe` on Windows and a `.deb` on Linux.

## What it offers

| Capability | What an agent can do |
|---|---|
| `terminal` | Open a PTY, run commands, read output or the rendered screen |
| `file_io` | Stat, list, read, write, edit, delete files and make directories |
| `input` | Type text, press chords, move the pointer, click, scroll |
| `screen_capture` | List displays and take screenshots — the coordinate space `input` works in |
| `file_transfer` | Send a file to another of your machines, peer to peer |
| MCP tools | Whatever your local MCP servers expose, promoted to first-class tools |

`input` and `screen_capture` share one coordinate space and are announced together: a position
read off a screenshot goes straight into `move_to`. Neither is announced when there is no display
server to reach, because a tool that is always going to fail is worse than a tool that is absent.

**Today `terminal` and `file_io` are live; the rest are still being written.** Between them that
is sixteen tools, and a capability is all or nothing — announcing `file_io` announces `remove`,
and announcing `terminal` announces `exec` with whatever command an agent chooses. A path an
agent sends without a leading slash starts in your home directory. That is where relative paths
start rather than a fence around them: an absolute path goes where it says, and a command can
work anywhere you can. What bounds this is the pause switch, the audit log, and what Attacca
lets an agent call in the first place.

The first time it runs, the window shows a short code and a link. Open the link, approve the code
in your browser, and Zyris connects this machine to your account. From then on it reconnects on
its own every time it starts, with no window required.

## Voice

Speech runs on this machine. Whisper transcribes, Supertonic speaks, and the models are fetched
on first run rather than shipped in the installer.

- **Hold the hotkey and talk.** Everything you say is a command. No wake word, no false triggers.
- **Or leave the microphone on** and call it by name. The wake word is one you record yourself,
  so it is a sound rather than a phrase in a particular language.
- **When the agent asks you something,** answer without calling it — that window opens on its own
  and closes when you reply.

Replies are spoken sentence by sentence as they stream in, so the wait is only ever for the first
one. Code blocks are read as "code" and parenthetical asides are skipped, because an answer read
aloud is not the same text as an answer on screen. Start talking and it stops to listen; what it
had not yet said does not go into the transcript.

## Building

You need a Rust toolchain, [Node](https://nodejs.org) and [pnpm](https://pnpm.io), and the
system libraries Tauri builds against — `pkg-config` looks for `webkit2gtk-4.1`,
`javascriptcoregtk-4.1`, `libsoup-3.0`, `gtk+-3.0`, `glib-2.0`, `gdk-pixbuf-2.0`, `cairo`,
`pango`, `harfbuzz`, `atk`, `librsvg-2.0`, `zlib` and `openssl`. The Linux tray additionally
needs `libayatana-appindicator` at runtime. On Debian/Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev \
    libgtk-3-dev libglib2.0-dev libgdk-pixbuf-2.0-dev libcairo2-dev libpango1.0-dev \
    libharfbuzz-dev libatk1.0-dev librsvg2-dev zlib1g-dev libssl-dev \
    libayatana-appindicator3-1
```

Other distributions name these packages differently.

The frontend has to be built before the Rust crate: `tauri.conf.json` points `frontendDist` at
`ui/dist`, which is not committed.

```bash
pnpm install
pnpm --filter zyris-ui build
```

Then, from the workspace root:

```bash
pnpm tauri dev                        # the window, with the dev server and hot reload
cargo run -p zyris-app -- --headless  # no window, no tray
cargo test                            # the frontend has to be built first, same as above
```

**`cargo run` on its own never shows the interface.** Tauri decides between the dev server and
the embedded assets from one cargo feature, not from the profile — `tauri::is_dev()` is
`!cfg!(feature = "custom-protocol")`. Without that feature the app loads the frontend from
`devUrl` (Vite on `localhost:5173`) and shows a connection error when nothing is serving there,
in release builds just as much as in debug ones.

So the window comes up through the Tauri CLI, or through cargo with the feature named:

```bash
pnpm tauri dev                                        # starts Vite first, then the app
pnpm tauri build                                      # a .deb or .exe, assets embedded
cargo run --release --features custom-protocol -p zyris-app   # the same, without the bundler
```

## Status

Early. The design is settled and the code is being written — see the roadmap below for what
lands in what order. Nothing here is ready to install yet.

1. Skeleton — workspace, tray, headless mode
2. Connection — enrollment, credential storage, reconnect (done)
3. Tools — terminal, files, pause switch, audit log (done); input and screen capture still to come
4. Autostart — Windows Task Scheduler, systemd user units, installers
5. File transfer — peer endpoint, fingerprint confirmation, inbox
6. MCP — local servers promoted to capabilities
7. Voice in — audio, echo cancellation, wake word, transcription
8. Voice out — streaming speech, interruption

## Security

This app hands a remote agent a shell, your files, and your keyboard. Two things stay on this
side of the connection regardless of what the server says:

- **A pause switch**, in the tray and on the Tools tab. While it is on, no new tool call is
  accepted, and the agent is told the machine is paused rather than that its tool broke. It
  stops new calls only: a command already running and a stream already open finish.
- **An audit log** of what ran — every call, allowed or refused, with what it was asked to touch
  but never what it read or wrote. On disk as one JSON line each, and as a tail on the Tools tab.

File transfers from a machine you have not seen before are refused until you compare the
fingerprint yourself. With no window to ask in, the answer is no.

## License

Apache-2.0. See [LICENSE](LICENSE).
