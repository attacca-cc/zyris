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
| `file_io` | Stat, list, read, write and edit files |
| `input` | Type text, press chords, move the pointer, click, scroll |
| `screen_capture` | List displays and take screenshots — the coordinate space `input` works in |
| `file_transfer` | Send a file to another of your machines, peer to peer |
| MCP tools | Whatever your local MCP servers expose, promoted to first-class tools |

`input` and `screen_capture` share one coordinate space and are announced together: a position
read off a screenshot goes straight into `move_to`. Neither is announced when there is no display
server to reach, because a tool that is always going to fail is worse than a tool that is absent.

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
cargo run -p zyris-app              # the window
cargo run -p zyris-app -- --headless  # no window, no tray
cargo test                          # the frontend has to be built first, same as above
```

## Status

Early. The design is settled and the code is being written — see the roadmap below for what
lands in what order. Nothing here is ready to install yet.

1. Skeleton — workspace, tray, headless mode
2. Connection — enrollment, credential storage, reconnect
3. Tools — terminal, files, input and capture, pause switch, audit log
4. Autostart — Windows Task Scheduler, systemd user units, installers
5. File transfer — peer endpoint, fingerprint confirmation, inbox
6. MCP — local servers promoted to capabilities
7. Voice in — audio, echo cancellation, wake word, transcription
8. Voice out — streaming speech, interruption

## Security

This app hands a remote agent a shell, your files, and your keyboard. Two things stay on this
side of the connection regardless of what the server says:

- **A pause switch** in the tray. While it is on, every tool call is refused.
- **An audit log** of what ran, readable in the window and on disk.

File transfers from a machine you have not seen before are refused until you compare the
fingerprint yourself. With no window to ask in, the answer is no.

## License

Apache-2.0. See [LICENSE](LICENSE).
