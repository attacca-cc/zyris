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
has yet called one of their tools end to end.** Between those five that is twenty-five tools —
an MCP server adds however many its own tools come to, on top — and a capability is all or
nothing: announcing `file_io` announces `remove`, and announcing `terminal` announces `exec` with
whatever command an agent chooses. A path an agent sends without a leading slash starts in
your home directory. That is where relative paths start rather than a fence around them: an
absolute path goes where it says, and a command can work anywhere you can. What bounds this is the
pause switch, the audit log, and what Attacca lets an agent call in the first place.

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

A machine is named by its node path, `system/program/node`: this app is always `<machine>/zyris/
desktop`, so sending to your laptop is sending to `laptop/zyris/desktop`, and what it sends here
lands in a folder called `laptop_zyris_desktop`. The Status screen shows this machine's own path.

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

**A machine enrolled before the switch to credentials is asked once more.** What it kept was an
account grant and a node token, and Attacca no longer accepts either: it now issues one credential
per program on each machine, and that is what Zyris keeps. The old pair is refused at the first
dial, thrown away, and the code comes back up.

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
    { "name": "calendar", "command": "calendar-mcp", "args": ["--ics", "/home/you/cal.ics"],
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

**On Windows that means `command` has to name the file that actually exists.** A great many MCP
servers are published to npm and started with `npx`, and on Windows `npx` is `npx.cmd` — a batch
wrapper, not a program. Because nothing goes through a shell, the bare name does not find it and
the server does not start; write the extension out:

```json
{ "name": "calendar", "command": "npx.cmd", "args": ["-y", "@example/calendar-mcp"] }
```

The same goes for `pnpm`, `yarn` and anything else that ships as `.cmd`. On Linux and macOS the
bare `npx` is right and the extension would be wrong. When it is wrong, the MCP tab shows that
server as not started, with the operating system's own "cannot find the file" as the reason, and
nothing else on the machine is affected.

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
one difference worth knowing.** When a call finishes, the log records that an MCP tool was called —
when, which server, which tool, and whether the call was allowed, refused or failed — and not what
was asked of it. Zyris writes down some arguments for its own capabilities because it knows what
they mean: `file_io`'s `path` is a file on this machine, `terminal`'s `command` is a command line.
A field spelled `path` on a server somebody else wrote is a coincidence of spelling and could as
easily be a password, so nothing an agent sends to an MCP server is written down.

**A call that never finishes is not written down at all.** Zyris puts no time limit on an MCP
server, so one that accepts a request and goes quiet waits until the agent gives up or the
connection drops; the line is written when a call returns, and a call cut off before it returns
never reaches it. Choosing a limit here would mean Zyris putting a clock on somebody else's tool,
and a build or a fetch that was working would be the thing it cut off. This is not special to MCP:
`terminal`'s `exec` with no `timeout_ms` has the same property, and so does any call an agent
abandons.

## Voice

Speech runs on this machine. Whisper transcribes, and the model is fetched on first run rather
than shipped in the installer.

**Nothing listens until you turn it on.** The switch is on the Voice tab. Opening a microphone
and downloading 141 MB are not things to do to somebody who has not asked for either, and a
feature that never starts on its own is not better — so it is asked once and then remembered:
the answer is kept with the rest of this instance's settings and Zyris starts listening again the
next time it runs. A run started with `--headless` never listens, because it has no window and no
push-to-talk key for anybody to hold.

- **Hold the hotkey and talk**, or say the wake word you recorded. A turn starts when the key goes
  down and ends when it comes up — or, after the wake word, when you stop talking.
- **While the switch is on** the microphone is open and nothing is recorded: the recording is gone
  once it has been turned into text.

**What you say goes to an agent on Attacca, and its answer is read aloud as it is written.** The
first time this machine connects with the voice on, Zyris makes a session for it — with the one
agent on your account, or, if there are several, the one named under `agent` in `voice.json` in
this computer's data directory — and keeps using it. If that chat is deleted on Attacca, the next
connection makes a new one. The Conversation tab shows both sides as they happen.

**Korean and English are both understood and both spoken.** Whisper works out which language each
turn is in, and the voice reads each sentence in the language its script says: Hangul as Korean,
kana as Japanese, anything else as English. A test reads a Korean and an English sentence aloud,
takes them through the same resampling a microphone goes through, and requires each to come back
word for word — but that is the voice being heard by whisper, not a person by either.

**Pressing the key while an answer is being read stops it**, and stops the agent writing it. What
you say next is sent with a note in front of it saying how much of the answer you heard, so the
agent knows where you cut in. Answers are read aloud only while listening is on.

### The push-to-talk key

`Ctrl+Alt+Space`, on Windows and on an X11 session, where an application is allowed to ask for a
key.

**A Wayland session is different and Zyris cannot bind the key for you.** It registers a global
shortcut called `push_to_talk` through the GlobalShortcuts portal, and which key points at it is
your compositor's business — version 1 of that interface gives an application no way to choose
one or even to offer you the choice. On Hyprland the line is

```
bind = CTRL ALT, space, global, :push_to_talk
```

and the Voice tab shows it, for the desktops whose spelling has actually been checked. On any
other it names the shortcut and leaves the line to you rather than guessing: a wrong line pasted
into a configuration file costs an evening. Zyris also cannot tell whether you have bound one —
your compositor does not say.

**Some desktops cannot do this at all.** XFCE, MATE, Cinnamon and LXQt fall back to
`xdg-desktop-portal-gtk`, which implements no GlobalShortcuts interface, so no application can
register a global key there. The Voice tab says so instead of offering a switch that could never
work.

**Not yet confirmed on Wayland: whether letting go of the key gets through.** The portal has a
signal for the release, Zyris listens for it, and it has not been possible to check here —
dispatching the shortcut by hand sends a press and never a release, so it takes a person holding
a real key. If a turn does not end when you let go, Zyris ends it after 30 seconds and throws the
recording away rather than sending half a sentence on. The check is written out under
[What nobody has checked by hand](#a-microphone-a-key-and-windows), with the rest of what is
owed.

### The wake word

**The phrase is "Hey Zyris"**, or whatever `"wakePhrase"` says in `voice.json` in this computer's
data directory — `"wakePhrase": "자이리스"`, say. It is on once you record takes of it on the Voice
tab (five, kept as 16 kHz WAV files beside a manifest), or once a phrase is typed there. When
listening starts, whisper reads the takes expecting the phrase, and the log line reading
`listening for the wake word` says how many of them it heard it in; none means the phrase you
recorded is not the one being listened for.

**Say the phrase and a turn starts** as the key would, ending when you stop talking. **Or say the
request straight after it** — "Hey Zyris, 오늘 날씨 알려줘" — and it is sent as it is. Everything said
while no turn is open is transcribed to find out, expecting the phrase, and it counts only when it
*starts* with it — allowing a word of lead-in, a letter in four spelled differently, and a Korean
vocative on the end of it ("자이리스야"). It is listened for only while listening is on and nothing
is being read aloud, and new takes count from the next time listening is turned on.

**Why words and not sound, and why told what to expect.** Matching the takes as sound (dynamic
time warping over MFCCs) could not tell the phrase from ordinary talk on a real enrolment: the
takes sat 17.9 apart from each other at the median, the phrase said live at 20.7 to 24, other
speech at 26 to 32. Transcribing instead, with no hint, whisper wrote the same five takes of "Hey
Zyris" as five different words in each of English, Korean and Japanese — it has never seen the
name. Told to expect it, it wrote all five as exactly "Hey Zyris", while "Hey Siri", "하이 자비스"
and "Hey, I see" kept their own words; a test holds both halves.

### The model

**Three speech models, chosen on the Voice tab, each downloaded once** into this computer's cache
directory (`~/.cache/zyris/models` on Linux, `%LOCALAPPDATA%\attacca\zyris\cache\models` on
Windows) rather than into the installer:

| Model | Download | On an i5-10400F, a 3-4 s sentence | |
|---|---|---|---|
| Base | 141 MB | 0.4-0.5 s | the default; gets names and borrowed words wrong ("기토부" for 깃허브) |
| Small | 465 MB | 1.6-2.1 s | mostly right |
| Large v3 Turbo (q5_0) | 547 MB | 9-10 s | right, and slow without a GPU, which this build does not use |

Each is checked against the SHA-256 published for it at one pinned revision before it is put in
place, so an interrupted or intercepted download leaves nothing behind. Choosing a model takes
effect at once if listening is on; the choice is `"speechModel"` in `voice.json`. If you already
have a `ggml-*.bin`, set `ZYRIS_WHISPER_MODEL` to it and that file is used whatever is chosen —
Zyris takes it as given, and will neither replace it nor delete it.

**The microphone and the speaker are both chosen there too**, or left to follow whatever this
computer calls the default. Answers are read through the chosen speaker while listening is on.

### The voice

**Speaking aloud needs a second model, 401 MB, downloaded the same way** and into the same
directory: [Supertonic 3](https://huggingface.co/supertone-oss-archive/supertonic-3), sixteen
files — four ONNX graphs, two small configuration files and ten voice styles — each pinned to one
published revision and each checked against its own SHA-256 before it is put in place. Set
`ZYRIS_TTS_MODELS` to a directory you have already unpacked it into and nothing is downloaded.

**The voice weights are not Apache-2.0.** Supertonic's example code is MIT and its **weights are
licensed BigScience OpenRAIL-M**: free to use and to redistribute, including commercially, with
use-based restrictions that must be passed on with any copy. Zyris does not ship them — they are
fetched onto your machine on request — but anyone redistributing a build with the models beside
it is redistributing those weights and takes that licence with them.

**Speech needs a CPU with AVX2** — Intel Haswell or AMD Excavator, 2013 and later. The
transcription engine is compiled without `-march=native` so that the release runs on every such
machine rather than only on the one that built it; on anything older it will not start. Nothing
else in Zyris needs it — but the published installers are built with speech in them, so in
practice it is a requirement of the whole application. See [Install](#install).

## What nobody has checked by hand

Everything on this page has tests behind it, and two kinds of claim are outside what any test on
this machine can reach: **an agent on Attacca**, and **a person at a real keyboard and a real
microphone, on a desktop that is not this one**. What follows is both lists, written down rather
than performed. **None of it has been run.** They are together so they can be done in one sitting.

### An agent, and a promoted MCP tool

The MCP path is tested up to and including a real server started from a real `mcp-servers.json`,
announced on a live node, and called by a peer on the other end of a real connection. **What no
test on this side can reach is Attacca.** Whether an agent that was never told an MCP server
exists picks a promoted tool out of the list and calls it like any other is a claim only a person
can check, and **nobody has checked it.**

It needs one enrolled machine and one stdio MCP server you already trust — whichever you run
today; nothing here depends on which. **No second computer is involved anywhere in it**, unlike
the file-transfer check that roadmap step 5 left owed; if you are running that one too, this rides
along with it on the same machine and the same agent. What it does need throughout is an agent on
Attacca talking to this node: steps 3, 4, 6 and 7 are all asked of the agent, and only steps 1, 2,
5, 8 and 9 happen entirely on this computer.

Step 5 reads the audit log. Zyris writes it to `~/.local/share/zyris/audit.jsonl` on Linux and
`%APPDATA%\attacca\zyris\data\audit.jsonl` on Windows — beside the `mcp-servers.json` described
under **MCP servers** above. A `--server` run keeps its own, in a `zyris-dev-…` directory of its
own rather than in `zyris`.

1. **Configure it.** Put one entry in `mcp-servers.json`, then **restart Zyris** — the file is
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

### A microphone, a key, and Windows

Speech is tested as far as a file can stand in for a microphone: a recording goes in at the
48 kHz stereo an ordinary device delivers, through the real rate conversion, the real silence
rule and real whisper, and the sentence comes back out of the same `VoiceEvent` the window
renders. What no test here has is a microphone, a finger on a key, a Wayland compositor, or a
Windows machine. Ten things are owed, and **the first can still change what the product is.** The
last four are about speaking rather than hearing, and need an agent on the Attacca account: the
session is made for you the first time the voice connects.

1. **Does letting go of the key get through? — Wayland only, and it is the one that matters.**
   Bind the shortcut as the Voice tab describes, turn listening on, hold the key for about two
   seconds while saying a sentence, then let go. Three times, unhurried.
   Pass: each hold ends *when you let go* — the window leaves Listening within a moment and the
   text arrives.
   Fail: nothing happens when you let go, and about thirty seconds later the turn is thrown away
   saying the key was held for more than thirty seconds without coming up. That is the portal
   never delivering the release.
   **If it fails, hold-to-talk is impossible on a Wayland session** and the interaction has to
   change — press to start and press again to stop is a different product, not a bug fix.
   Nothing else depends on it: an X11 session and Windows both report the release themselves.
   The Voice tab says today that this is unconfirmed; if it passes, that sentence should go.
2. **With the window closed.** Close the window to the tray, hold the key, say something, and
   open the window again. Pass: the transcript is there and the microphone never stopped —
   speech lives in the core and the window is a reader of it, which is the claim. A run started
   with `--headless` is the opposite case and is meant to listen to nothing at all: it has no
   key for anybody to hold.
3. **Windows: one hold is one turn.** Hold the key for about five seconds and say a sentence in
   the middle of it. Pass: the window enters Listening once and the whole sentence comes back as
   one transcript. Fail: several turns, or a transcript that begins in the middle of what you
   said — which would mean a held key is repeating. Windows is asked not to repeat the hotkey
   while it is held; that request has been read in the source and never seen work. Tap the key
   on its own too: a tap should come back as "nobody spoke", not as a turn that never ends.
4. **Windows: the microphone, including a refused one.** Pass: the Voice tab lists devices with
   readable names, and the one you pick is the one that records. Then turn microphone access off
   for desktop applications in Windows' privacy settings and turn listening on. Pass: Zyris says
   Windows has not given it access and offers the settings page. Fail: any other wording, and
   especially a bare error number — the sound library does not classify a refusal, so Zyris reads
   the message text, and the spellings it looks for were read out of the Windows sources rather
   than produced by a real refusal.
5. **A wake word take while listening is on.** Turn listening on, then record the five takes on
   the same tab, and hold the push-to-talk key between them. Pass: every take records and the key
   goes on producing turns. Fail: a take that will not start, a take that comes back silent, or a
   key that stops working afterwards — recording a take opens a **second** input stream on the
   same device without closing the first, and no machine has been asked to do that yet.
6. **Korean, heard.** Say something in Korean and read the transcript. Whisper detects the
   language of each turn now, because told `en` it did not mishear Korean, it replaced it with a
   different English sentence. What is measured is the voice reading Korean and English sentences
   to whisper through the microphone's resampler, and each coming back word for word (0.37 s for
   a three-second sentence, detection included, on an i5-10400F). **A person's voice, accent and
   room have not been tried**, and `ggml-base` is a small model: whether that is good enough for
   ordinary Korean speech, or needs a larger one, is what this check decides.

7. **An answer read aloud at all.** Turn listening on and ask an agent something with a few
   sentences in the reply.
   Pass: it starts speaking while the text is still arriving, and stops at the end.
   Fail: silence, or it waits for the whole answer first.
   **Gaps between sentences depend on the machine.** Synthesis ran slower than speech on the
   development machine — a real-time factor of 1.2 to 1.9 — so the speaker caught up with the
   queue and waited; on an i5-10400F it runs at about 0.25, four times faster than speech.

8. **Korean, spoken.** Ask for an answer in Korean and listen to it. Each sentence with Hangul in
   it is read with the Korean tag, and whisper transcribes what the voice makes of one exactly —
   so it is intelligible Korean to a machine. **No person has listened to it**, and whether it
   sounds natural is what this check is for.

9. **Interrupting it.** While it is speaking, or while the agent is still thinking, press and
   hold the push-to-talk key and say something.
   Pass: the speaking stops at once, nothing more of that answer is read, and on Attacca your
   message arrives as **one** message — a bracketed note saying which sentences were heard,
   which was cut off and how far in, and then your words — answered by **one** reply.
   Note what this is *not*: **an answer the agent had already finished writing is not
   shortened**, because the protocol's cancel carries no delivery point — [an issue asking for
   one](https://github.com/attacca-cc/zyris-protocol/issues/42) is open. The note is the whole of
   the repair.
   **The key is the only way to interrupt, and that is a decision rather than a limitation of the
   microphone.** Windows builds have no echo canceller wired in, and the one Linux ships has been
   measured on a synthetic echo and never in a room; a microphone allowed to interrupt would risk
   cutting the answer off on its own first word.

10. **Both models at once.** With listening on and an answer being spoken, look at how much memory
    Zyris is using. Measured separately: about 213 MB for the listening half and 451 MB for the
    speaking half. **Nobody has run them together**, and the development machine has 3.6 GB with a
    browser engine also resident.

**The Windows echo canceller is not one of the six, and that is deliberate.** Windows cannot build
the echo canceller Linux uses, so it has one of its own — the Voice Capture DSP that ships with
Windows itself. It has been built and measured on Windows 11: **56 dB of a synthetic echo removed
with the loudspeaker signal fed to it, against 4 dB without**, holding steady over twenty seconds.
But that echo is a delayed copy of the loudspeaker and nothing else — no room, no reflections, no
second voice — and **nothing is wired to it yet**, so there is nothing on any screen to check and
no way to try it. What it is owed is a real room, and that goes on this list the day the microphone
actually goes through it.

## Install

Every `v*` tag builds the installers on GitHub's runners and attaches them to a release, so the
downloads are on the [releases page](https://github.com/attacca-cc/zyris/releases): a
`Zyris_<version>_amd64.deb` for Debian and Ubuntu, a `Zyris_<version>_x64-setup.exe` for
Windows. **Nothing is tagged yet** — see Status below for what is still being written.

**These need a CPU with AVX2 — Intel Haswell or AMD Excavator, 2013 and later — and on anything
older Zyris will not start at all.** Not the speech alone: the transcription engine is compiled
into the binary, so an older machine gets a process that dies before its first window, with no
message. Nothing else in Zyris has that requirement, and a build without `--features voice`
would not either; the published installers carry it because an app that cannot be spoken to is
not this one.

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

**Speech is a feature of its own, `voice`**, and a build without it has no microphone and no
voice. It compiles whisper.cpp through CMake, generates bindings with `bindgen` (so `libclang`
has to be findable) and links ONNX Runtime, which `ort` downloads once per machine:

```bash
cargo run --release --features custom-protocol,voice -p zyris-app
```

**whisper.cpp is compiled for the AVX2 set by `.cargo/config.toml`**, for every build in this
workspace. `GGML_NATIVE=OFF` keeps a release from depending on the CPU that built it, and on its
own it also turns every instruction set off, which makes transcription about ten times slower;
the lines beside it put AVX2 back. A release with the voice refuses to build without them.
`whisper-rs-sys` does not rebuild when these change, so after changing one run
`cargo clean -p whisper-rs-sys` (with `--release` for that profile).

**`gpu` runs transcription on the graphics card through Vulkan**, on any vendor's GPU. It needs
the Vulkan headers, the loader and `glslc` (shaderc) to build, and the binary then needs a Vulkan
loader to start. On an RTX 3050, Large v3 Turbo goes from about ten seconds a sentence to a
quarter of a second. The first load after a build compiles its shaders, which takes about ten
seconds once; the driver keeps them after that.

```bash
cargo run --release --features custom-protocol,gpu -p zyris-app
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
   [check against a real agent](#an-agent-and-a-promoted-mcp-tool))
7. Voice in — audio, wake word recording, transcription (done, except for the
   [checks that need a person](#a-microphone-a-key-and-windows))
8. Voice out — streaming speech, interruption (done, except for the
   [checks that need a person](#a-microphone-a-key-and-windows); on Windows the echo canceller
   is built and reaches nothing yet)

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
