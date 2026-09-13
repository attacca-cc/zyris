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

**Today `terminal`, `file_io`, `screen_capture`, `input` and `file_transfer` are live; MCP is
still being written.** Between them that is twenty-five tools, and a capability is all or
nothing — announcing `file_io` announces `remove`, and announcing `terminal` announces `exec`
with whatever command an agent chooses. A path an agent sends without a leading slash starts in
your home directory. That is where relative paths start rather than a fence around them: an
absolute path goes where it says, and a command can work anywhere you can. What bounds this is
the pause switch, the audit log, and what Attacca lets an agent call in the first place.

`input` and `screen_capture` are announced together or not at all — an agent that can see the
screen but not act on it is half useful, and one that can act but not see is guessing
coordinates. On Linux, neither appears when no display server answers. On Windows they are
always announced: the layer underneath reports success without checking, so there is nothing
to detect, and on a Windows session with no interactive desktop the pointer calls will look
like they worked. Positions are in the pixels a screenshot actually returned, which on a scaled
display is not what your settings panel says; a coordinate read off a capture goes straight
into `move_to` with nothing applied to it.

The audit log records which display and where the pointer went. It does not record what was
typed: `type_text` is how a password reaches an application, and a run of single-key presses
reconstructs one just as well, so neither the text nor its length is written down.

## Sending a file to another of your machines

`file_transfer` moves a file straight between two of your computers rather than through Attacca.
The bytes travel over [iroh](https://iroh.computer), and what arrives lands in an inbox under a
folder named after the machine that sent it.

**The two directions are not gated the same way, and it is worth knowing which is which.**

*Receiving* is gated on your account rather than on an approval. An arriving connection is
authenticated against the sending machine's own key, and that key has to belong to a node of your
Attacca account: Zyris asks Attacca for the account's node list and closes anything not on it
before a word is exchanged. So **any machine you have enrolled can send this one a file**, and
nothing else can. Zyris does not ask you first, there is no per-machine approval on this side, and
the window will not add one — what stops a machine of your own sending here is revoking that node
on your account.

*Sending* is gated on a pin. Before this machine sends to a name for the first time, someone has
to confirm the key behind that name, and **there is no way to confirm one from the window yet** —
that is the next piece of work. Until it lands a peer has to have been pinned already, so sending
is useful between machines you have set up and not yet useful for a machine you just added.

Once a name is pinned, the pin keeps working in both directions: a key that is not the one pinned
for that name is refused, whether this machine is dialling it or it is dialling here. What that
does not cover is a name nothing is pinned under — and on the receiving side the name comes from
Attacca rather than from you, so a node of your account that arrives under a name you have never
sent to is simply let through, and stays unpinned.

Each computer keeps a long-lived key so it stays the same peer across restarts. That key is what
a pin is a pin *of*; lose it and every machine that pinned this one refuses to send to it.

Without a relay of your own, the connection rides the public ones run by the iroh project. **A
relay cannot read what is transferred** — it is encrypted end to end — but it does see which of
your machines talked to which, and when. Set `ZYRIS_RELAY_URL` to point at your own relay
instead.

The first time it runs, the window shows a short code and a link. Open the link, approve the code
in your browser, and Zyris connects this machine to your account. From then on it reconnects on
its own every time it starts, with no window required.

It can start itself, too — a Task Scheduler entry on Windows, a systemd user unit on Linux,
turned on from the Settings screen or with `zyris --install-autostart`. Started that way it puts
no window on the screen: click the tray icon to get one, or just launch Zyris again. **On Linux
that means when you log in to a desktop, not when the computer boots.** The window and the tray
icon need a graphical session to start into, so a Linux machine that is switched on with nobody
logged in is not connected.

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

## Install

Every `v*` tag builds the installers on GitHub's runners and attaches them to a release, so the
downloads are on the [releases page](https://github.com/attacca-cc/zyris/releases): a
`Zyris_<version>_amd64.deb` for Debian and Ubuntu, a `Zyris_<version>_x64-setup.exe` for
Windows. **Nothing is tagged yet** — see Status below for what is still being written.

```bash
sudo apt install ./Zyris_0.1.0_amd64.deb
```

**The Windows installer is not signed, and Windows will say so.** SmartScreen shows "Windows
protected your PC" and puts the install button behind **More info → Run anyway**; Defender
SmartScreen in Edge will offer to discard the download for the same reason. That is about the
absent certificate rather than about the installer. Signing needs a certificate nobody on this
project has, and a build everyone can produce beats a signing step that fails for everyone who
forks the repository.

## Building

You need a Rust toolchain, [Node](https://nodejs.org) and [pnpm](https://pnpm.io), and the
system libraries the build links against — `pkg-config` looks for `webkit2gtk-4.1`,
`javascriptcoregtk-4.1`, `libsoup-3.0`, `gtk+-3.0`, `glib-2.0`, `gdk-pixbuf-2.0`, `cairo`,
`pango`, `harfbuzz`, `atk`, `librsvg-2.0`, `zlib`, `openssl` and `libpipewire-0.3`, and the link
also needs `libgbm`. The Linux tray additionally needs `libayatana-appindicator` at runtime. On
Debian/Ubuntu:

```bash
sudo apt install libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev \
    libgtk-3-dev libglib2.0-dev libgdk-pixbuf-2.0-dev libcairo2-dev libpango1.0-dev \
    libharfbuzz-dev libatk1.0-dev librsvg2-dev zlib1g-dev libssl-dev \
    libayatana-appindicator3-1 libpipewire-0.3-dev libgbm-dev
```

Other distributions name these packages differently.

The last two are for `screen_capture` rather than for Tauri: `xcap` reaches the screen through
`libwayshot`, which brings PipeWire and GBM with it. Without `libpipewire-0.3-dev` the
`libspa-sys` build script stops at `Package 'libpipewire-0.3' ... not found`; without
`libgbm-dev` the whole workspace compiles and then the link of `zyris-tools` fails with
`unable to find library -lgbm`. The Wayland, X11, DRM and EGL libraries that path also wants
arrive as dependencies of packages already on that line, so there is nothing further to add.

**That list is the one CI installs, and it was arrived at by building rather than by guessing** —
three earlier guesses at it were wrong. `.github/workflows/ci.yml` builds and runs
`cargo test --workspace` on `ubuntu-latest` and `windows-latest` on every push and pull request,
so the list stays honest: the day it stops being enough, the Ubuntu job goes red.

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
cargo run -p zyris-app -- --minimized # the tray only; what autostart installs
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
3. Tools — terminal, files, keyboard, mouse, screen capture, pause switch, audit log (done)
4. Autostart — Windows Task Scheduler, systemd user units (at desktop login, not at boot), installers (done)
5. File transfer — peer endpoint, inbox (done); confirming a new peer's key from the window, so
   this machine can send to one it has not pinned, still to come
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

A file can only arrive from a machine enrolled on your own Attacca account: a peer whose key is
not on the account's node list is closed before the two ends have said anything to each other.
That is the whole of the check on this side — **an incoming file is not something you are asked
about**, and a machine of yours that you have never pinned can still send you one. The pin gates
the other direction, and with no window to confirm a new key in, sending to a machine this one
has not already pinned is refused.

## License

Apache-2.0. See [LICENSE](LICENSE).
