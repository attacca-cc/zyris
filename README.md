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

**Today `terminal`, `file_io`, `screen_capture`, `input` and `file_transfer` are live, and so is
the promotion of local MCP servers — see [MCP servers](#mcp-servers) — though no agent on Attacca
has yet called one of their tools end to end.** Between them that is twenty-five tools, and a
capability is all or nothing — announcing `file_io` announces `remove`, and announcing `terminal`
announces `exec` with whatever command an agent chooses. A path an agent sends without a leading
slash starts in your home directory. That is where relative paths start rather than a fence around
them: an absolute path goes where it says, and a command can work anywhere you can. What bounds
this is the pause switch, the audit log, and what Attacca lets an agent call in the first place.

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
folder named after the machine that sent it — `~/.local/share/zyris/inbox` on Linux,
`%APPDATA%\attacca\zyris\data\inbox` on Windows. The Tools tab lists what is in there, newest
first, with each file's full path.

**The two directions are not gated the same way, and it is worth knowing which is which.**

*Receiving* is gated on your account rather than on an approval. An arriving connection is
authenticated against the sending machine's own key, and that key has to belong to a node of your
Attacca account: Zyris asks Attacca for the account's node list and closes anything not on it
before a word is exchanged. So **any machine you have enrolled can send this one a file**, and
nothing else can. Zyris does not ask you first, there is no per-machine approval on this side, and
the window will not add one — what stops a machine of your own sending here is revoking that node
on your account.

*Sending* is gated on a pin, and that is the one you are asked about. The first time this machine
sends to a name, the window comes up with the fingerprint of the key answering to that name and
waits for you. Compare it against what the other machine says about itself — open Zyris on that
machine and read its Status screen, under **This computer's fingerprint** — and approve only if
the two match character for character. Approving pins that key under that name: this machine sends
there without asking from then on, and a *different* key under the same name is refused outright
rather than asked about a second time. Refusing pins nothing and fails that one send; an agent can
try again, and you will be asked again.

A machine running `--headless` has no window to read that off. Every Zyris writes the same value
to its log when it starts, on the line reading `peer identity ready`, so on a headless machine
that is where you look. It is not worth relying on anywhere else: Zyris logs to standard output,
and a copy started by the autostart entry has no console for that output to reach.

**A question cannot be replaced under your hand.** Only one machine is ever waiting to be
approved: a second one asking while you are being asked is refused outright rather than queued
behind you, and it stays refused for a moment after you answer, so that nothing can take the
screen in the instant your click is landing. Both answers are also dead for the first three
quarters of a second a question is on the screen. The whole point of a fingerprint is that
somebody read it, and a button you can be trained to click without looking is worth nothing.

**Nobody at the screen is a refusal.** The question gives up after 45 seconds, because the agent's
call is cut off at 55 and an answer after that reaches nobody. `--headless` refuses every unknown
peer without asking at all — there is nobody to ask, and nobody being around is not consent.

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
instead. It takes a whole URL, scheme and all:

```bash
ZYRIS_RELAY_URL=https://relay.corp.example:3340
```

A value that is not an `http` or `https` URL with a host in it is refused outright rather than
quietly falling back to the public relays: Zyris says so in the log, `file_transfer` is not
announced, and the rest of the machine carries on. `relay.corp.example:3340` — the same thing
without the scheme — is the spelling to avoid, and the one that used to be accepted and then
ignored.

The first time it runs, the window shows a short code and a link. Open the link, approve the code
in your browser, and Zyris connects this machine to your account. From then on it reconnects on
its own every time it starts, with no window required.

It can start itself, too — a Task Scheduler entry on Windows, a systemd user unit on Linux,
turned on from the Settings screen or with `zyris --install-autostart`. Started that way it puts
no window on the screen: click the tray icon to get one, or just launch Zyris again. **On Linux
that means when you log in to a desktop, not when the computer boots.** The window and the tray
icon need a graphical session to start into, so a Linux machine that is switched on with nobody
logged in is not connected.

## MCP servers

An MCP server you already run on this computer is promoted to a capability of it, so its tools sit
beside `terminal` and `file_io` and an agent calls them exactly the same way. A server you call
`desk-notes` is announced as `mcp_desk-notes`, and its `search` tool is `mcp_desk-notes.search`.

Which servers Zyris runs is a file you write:

- `~/.local/share/zyris/mcp-servers.json` on Linux
- `%APPDATA%\attacca\zyris\data\mcp-servers.json` on Windows

It does not have to exist. A machine without one runs no MCP servers, which is the ordinary state
of an ordinary machine and not something Zyris complains about.

```json
{
  "servers": [
    { "name": "desk-notes", "command": "notes-mcp", "args": ["--root", "/home/you/notes"] },
    { "name": "calendar", "command": "npx", "args": ["-y", "@example/calendar-mcp"],
      "enabled": false }
  ]
}
```

`name` and `command` are required, `args` defaults to none and `enabled` to true. **A field Zyris
does not recognise makes the whole file invalid**, on purpose: the mistakes a hand-edited file
collects are spelling ones, and an `"arg"` quietly ignored is a server that starts with none of
the arguments you gave it. Each command is run directly and spoken to over its standard input and
output — nothing goes through a shell, so each argument is passed exactly as written and none of
them is split or expanded.

Zyris reads this file when it starts and **never writes to it**. The MCP tab lists what is in it
and what each server is doing; the switches there stop and start a server for as long as Zyris is
running, and the file is what decides which servers come back after a restart.

**Two mistakes cost more than the entry they are in.** A file that will not parse starts no MCP
server at all — the MCP tab says why, the log says why, and nothing else on the machine is
affected. Two entries sharing a name do the same, because both would be announced under one
capability name, and a node that announces one name twice announces *nothing*: not `terminal`, not
`file_io`. Everything else costs only its own entry — a command that is not there, a command that
does not speak MCP, or a name with a dot in it, which can never be announced because an agent
addresses a tool as `capability.tool` and everything before the first dot is read as the
capability.

A server whose process goes away is withdrawn within about a second: its tools stop being
announced, and the MCP tab shows it as having stopped on its own rather than as one you turned
off. A server that starts and never answers is given ten seconds before Zyris gives up on it and
leaves it out.

**Promoted tools are behind the same pause switch and the same audit log as everything else, with
one difference worth knowing.** The log records that an MCP tool was called — when, which server,
which tool, and whether the call was allowed, refused or failed — and not what was asked of it.
Zyris writes down some arguments for its own capabilities because it knows what they mean:
`file_io`'s `path` is a file on this machine, `terminal`'s `command` is a command line. A field
spelled `path` on a server somebody else wrote is a coincidence of spelling and could as easily be
a password, so nothing an agent sends to an MCP server is written down.

### Checking it against a real agent — this has not been run

Everything above has tests behind it, up to and including a real MCP server started from a real
`mcp-servers.json`, announced on a live node, and called by a peer on the other end of a real
connection. **What no test on this side can reach is Attacca.** Whether an agent that was never
told an MCP server exists picks a promoted tool out of the list and calls it like any other is the
one claim on this page that only a person can check, and **nobody has checked it.** What follows
is the procedure, written down rather than performed.

It needs one enrolled machine and one stdio MCP server you already trust — whichever you run
today; nothing here depends on which. If you are also running the file-transfer check that step 5
left owed, this rides along with it: same machine, same agent, and only step 7 below wants a
second computer's attention at all.

1. **Configure it.** Put one entry in the file named above, then **restart Zyris** — the file is
   read at startup and never again.
2. **Look at the MCP tab.** Pass: the server is listed as running, with the tools it promoted and
   the capability name an agent will address (`mcp_<name>`). The log says the same thing on the
   line reading `an MCP server is promoted`. If it says the server failed, that is the reason, and
   there is nothing to check further until it starts.
3. **Ask an agent on Attacca what this machine can do**, without mentioning MCP. Pass: the reply
   names `mcp_<name>` beside `terminal` and `file_io`, with the server's own tool descriptions.
   Fail: the agent lists the built-ins only, or describes the promoted one as something it cannot
   use.
4. **Ask it to do something only that server can do.** Pass: the answer is the server's own, and
   the agent treats the call as ordinary — no "I do not have a tool for that", no asking you to
   run something. That is the whole claim on the front page.
5. **Read the audit tail on the Tools tab.** Pass: a line naming `mcp_<name>` and the tool, marked
   allowed, **with no arguments on it** — and the argument you actually sent appears nowhere in
   `audit.jsonl`. Fail, and it is the serious kind: an MCP server's arguments are being written to
   disk.
6. **Hit pause and ask again.** Pass: the agent reports that this machine is paused, in the same
   words it would use for `terminal` — not that the tool is broken or missing.
7. **Switch the server off on the MCP tab while the agent is connected**, then ask the agent what
   it can do. Pass: the capability is gone from its list within a moment, without either end
   reconnecting, and a call to it fails as not announced rather than hanging. Switch it back on
   and it comes back.
8. **Kill the server's process from outside Zyris** — Task Manager, or `kill` on Linux. Pass:
   within about a second the MCP tab shows it as having stopped on its own, *not* as one you
   switched off, and the capability is no longer announced to the agent.
9. **Misspell a field in the file and restart.** Pass: Zyris starts, the MCP tab names the problem
   and the path of the file, and `terminal` and `file_io` still work. Fail: Zyris does not start,
   or the tab says you have configured no servers.

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
5. File transfer — peer endpoint, inbox, approving a new machine's key from the window (done)
6. MCP — local servers promoted to capabilities (done, except for the
   [check against a real agent](#checking-it-against-a-real-agent--this-has-not-been-run))
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
  A tool from one of your [MCP servers](#mcp-servers) is recorded as having been called and
  without its arguments, because Zyris has no idea what they mean on somebody else's server.

A file can only arrive from a machine enrolled on your own Attacca account: a peer whose key is
not on the account's node list is closed before the two ends have said anything to each other.
That is the whole of the check on this side — **an incoming file is not something you are asked
about**, and a machine of yours that you have never pinned can still send you one. Nor does the
pause switch cover it: an arriving file asks this machine's agent surface for nothing, so there is
no call for the switch to stop. What the Tools tab shows you is what arrived, after the fact.

The pin gates the other direction, and that one you are asked about: before this machine sends to
a name it has not sent to before, the window shows you the fingerprint of the key answering to
that name and waits for you to approve it or refuse.

## License

Apache-2.0. See [LICENSE](LICENSE).
