import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { MAX_TOOL_CALLS, type Action, type State, type ToolCallRow } from "./state";

// What the `announced_tools` command answers with: `zyris_tools::Announcement`, serialized
// camelCase. A test in crates/zyris-tools/src/announce.rs pins those field names; this is the
// other half of that agreement, and nothing checks the two at build time.
type Announcement = {
  capabilities: { name: string; version: number; tools: string[] }[];
  root: string;
  auditLog: string;
};

// One file that has arrived, as the `inbox` command answers with it — `zyris_caps::InboxEntry`,
// read through the `file_transfer` capability rather than off the filesystem, so this list and an
// agent's `inbox_list` are the same read.
//
// **Serialized snake_case, unlike everything else crossing this boundary.** The type comes from
// the protocol stack and derives a plain `Serialize` with no `rename_all`; a test in
// crates/zyris-app/src/bridge.rs pins these names, and reaching for `receivedUnixMs` here would
// silently render every arrival at the epoch.
type InboxEntry = {
  from: string;
  name: string;
  bytes: number;
  path: string;
  received_unix_ms: number;
};

// A rejected `invoke` carries whatever the command returned as its error. These commands return
// strings or nothing at all, so anything else means the bridge itself broke and is not worth
// showing verbatim.
function asMessage(error: unknown, fallback: string): string {
  return typeof error === "string" ? error : fallback;
}

// The stored time is RFC 3339 with nanosecond precision (2026-09-10T18:55:44.287347208Z).
// JavaScript's date parser is only specified for three fractional digits, so the fraction is cut
// to three here rather than trusting the engine to tolerate nine — and nine digits in a row of a
// list is noise whatever the parser does. A time that will not parse is shown exactly as it was
// written; nothing here ever renders "Invalid Date".
function clockTime(at: string): string {
  const parsed = new Date(at.replace(/\.(\d{3})\d+/, ".$1"));
  if (Number.isNaN(parsed.getTime())) return at;
  return parsed.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

function toolCount(count: number): string {
  return count === 1 ? "1 tool" : `${count} tools`;
}

// When a file arrived, or null when this computer cannot say.
//
// `received_unix_ms` is epoch milliseconds — a number, so none of the audit tail's trimming
// applies — but it has a trap of its own. Upstream reads it with `Metadata::modified` and falls
// back to **zero** when the filesystem will not give a time, so 0 means "unknown" and rendering
// it would date every such file to January 1970. A value too large for a `Date` is the same kind
// of nonsense from the same source, and `getTime()` is NaN for it.
//
// A date and not just a clock, unlike the tool calls: the audit tail shows the last few minutes
// of a running machine, and an inbox holds whatever arrived over weeks.
function arrived(ms: number): { label: string; iso: string } | null {
  if (ms <= 0) return null;
  const at = new Date(ms);
  if (Number.isNaN(at.getTime())) return null;
  return {
    label: at.toLocaleString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    }),
    iso: at.toISOString(),
  };
}

// Decimal units, and the exact count stays on the row as its title. Rounding is the point of the
// short form — a person is reading "did the whole thing arrive", and 1.4 GB answers that better
// than 1413251072 does.
function fileSize(bytes: number): string {
  if (bytes < 1000) return bytes === 1 ? "1 byte" : `${bytes} bytes`;
  const units = ["kB", "MB", "GB", "TB"];
  let value = bytes / 1000;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

export function Tools({ state, dispatch }: { state: State; dispatch: (action: Action) => void }) {
  const [announcement, setAnnouncement] = useState<Announcement | null>(null);
  const [announcementProblem, setAnnouncementProblem] = useState<string | null>(null);
  const [stored, setStored] = useState<ToolCallRow[]>([]);
  const [callsProblem, setCallsProblem] = useState<string | null>(null);
  // What the inbox read answered, and the three answers it can give: the list, `null` for a
  // machine with no peer identity — where `file_transfer` is not announced, so no file can arrive
  // and there is no inbox to read — and `undefined` while the read is still in flight.
  //
  // Kept apart from `inboxProblem` for the reason the audit tail keeps `callsProblem` apart from
  // `stored`: a failed read is not an empty inbox, and only a read that came back empty may be
  // shown as one. That defect shipped once already, in this file.
  const [inbox, setInbox] = useState<InboxEntry[] | null | undefined>(undefined);
  const [inboxProblem, setInboxProblem] = useState<string | null>(null);
  const [switchProblem, setSwitchProblem] = useState<string | null>(null);

  // The newest call this window had already seen when the stored tail was asked for. Everything
  // above it in `state.toolCalls` arrived after that moment and is this screen's to render;
  // everything from it down is already in the tail read back from the file. `null` means there
  // is no such mark — either nothing had arrived yet, or the file could not be read — and then
  // every live call is rendered.
  const boundary = useRef<ToolCallRow | null>(null);

  useEffect(() => {
    // The same guard App.tsx's startup catch-up uses. Without it, StrictMode's second run of
    // this effect answers over the first run's answers — and for a list that live events are
    // prepended to, that is the durable tail rendered twice.
    let cancelled = false;

    // Snapshotted before the file is asked for, not when it answers. A call landing in that gap
    // is written to the file *and* delivered here, so it can appear twice for as long as this
    // screen stays open; cutting at the answer instead would drop such a call from the list
    // altogether. A duplicate is visible and a hole is not, and the file is the record either
    // way.
    boundary.current = state.toolCalls[0] ?? null;

    void invoke<Announcement>("announced_tools")
      .then((answer) => {
        if (!cancelled) setAnnouncement(answer);
      })
      .catch((error: unknown) => {
        if (!cancelled) {
          setAnnouncementProblem(asMessage(error, "Could not read what this computer offers."));
        }
      });

    void invoke<ToolCallRow[]>("recent_tool_calls", { limit: MAX_TOOL_CALLS })
      .then((rows) => {
        if (cancelled) return;
        // An empty answer beside a non-empty live list means the file did not get calls this
        // window did — an unwritable log, which `AuditLog::record` swallows so that a failing
        // log can never fail a tool call. There is no tail to cut against, so show every live
        // row rather than hiding the ones this window saw before the screen was opened.
        if (rows.length === 0) boundary.current = null;
        setStored(rows);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        // With no stored tail there is no boundary to draw, so show every live call.
        boundary.current = null;
        setStored([]);
        setCallsProblem(asMessage(error, "Could not read what ran on this computer."));
      });

    // What has arrived. Read through the capability by the command, so this is the same answer
    // an agent's `inbox_list` gets rather than a second walk of the same directories.
    //
    // **Read again every time the window comes back to the front**, which is the Settings screen's
    // idiom one file over and is here for a sharper version of the same reason. Nothing publishes
    // an event when a file lands: an incoming transfer does not come through the Attacca
    // connection at all — it is not a tool call, it is not behind the pause switch, and the event
    // bus never hears of it — so there is nothing to subscribe to and every read is a snapshot.
    //
    // A read on mount alone would be a snapshot from the *start of the run*, not from the last
    // visit to this tab. Closing the window hides it rather than tearing it down (`gui.rs`
    // prevents the close), so this component is never unmounted and its state outlives every
    // close and reopen: "Nothing has arrived yet." could sit there for days with files landing
    // underneath it. Focus is what covers that, because coming back to look is exactly the moment
    // somebody wants an answer that is current.
    function readInbox() {
      void invoke<InboxEntry[] | null>("inbox")
        .then((arrived) => {
          if (cancelled) return;
          setInbox(arrived);
          // An answer is an answer: a message from a read that failed earlier has stopped being
          // true, and leaving it above a fresh list reads as a broken screen.
          setInboxProblem(null);
        })
        .catch((error: unknown) => {
          if (cancelled) return;
          // Deliberately not `setInbox([])`. An unreadable inbox is not an empty one, and the
          // sentence below about nothing having arrived is reserved for a read that succeeded.
          setInboxProblem(asMessage(error, "Could not read what has arrived."));
        });
    }

    readInbox();

    let unlisten: UnlistenFn | undefined;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        // Only on the way in, like Settings. This fires on blur as well, and walking the inbox as
        // somebody clicks away spends a directory read on an answer nobody sees.
        if (focused) readInbox();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });

    // Where the switch is. The `paused` event is the usual way this arrives, but the core's
    // one-slot catch-up holds only the most recent event of any kind, so a window that opened
    // after something else was published would never have heard it. Folded back through the
    // reducer, because `state.paused` has to stay the only answer to the question.
    void invoke<boolean>("is_paused")
      .then((paused) => {
        if (!cancelled) dispatch({ kind: "paused", paused });
      })
      .catch(() => {
        // Nothing to say: the switch keeps whatever state the last event left, and the next
        // change publishes one.
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
    // Runs once, deliberately, and it is the *effect* that is mount-only rather than every read
    // inside it. The stored tail is a snapshot to sit beneath the live calls, and re-reading it
    // whenever a call arrived would move the boundary out from under those calls. The inbox is the
    // opposite case and re-reads itself on focus, above.
  }, []);

  // `indexOf` by identity: `reduce` prepends and keeps the existing row objects, so the mark
  // survives every later event until the cap evicts it. By the time it does, every row in the
  // list arrived after the mark anyway — which is what -1 means here.
  const cut = boundary.current ? state.toolCalls.indexOf(boundary.current) : -1;
  const live = cut === -1 ? state.toolCalls : state.toolCalls.slice(0, cut);
  const rows = [...live, ...stored].slice(0, MAX_TOOL_CALLS);

  function toggle() {
    setSwitchProblem(null);
    // Nothing is set here. The core publishes `paused` when the switch moves and that event is
    // what changes this screen — the same event the tray's menu item follows, so the two can
    // never end up showing different things.
    invoke("set_paused", { paused: !state.paused }).catch((error: unknown) => {
      setSwitchProblem(asMessage(error, "Could not move the switch."));
    });
  }

  return (
    <main className="screen screen-wide">
      <h1>Tools</h1>
      <p className="lead">
        What your Attacca agents can reach on this computer, what they have run, and what your
        other machines have sent here.
      </p>

      <section className="panel">
        <div className="switch-row">
          <p className="switch-state">
            <span className={state.paused ? "dot dot-off" : "dot dot-on"} aria-hidden="true" />
            {state.paused
              ? "Paused. No new commands or file access will be accepted."
              : "Running. Agents can run commands and read and write files as you."}
          </p>
          <button type="button" className="button" onClick={toggle}>
            {state.paused ? "Resume" : "Pause"}
          </button>
        </div>
        {/* The switch stops calls arriving; it does not reach into one that is already under
            way. Saying so is the whole point — a person who reads "paused" as "nothing is
            running" has been told something untrue. See zyris_tools::gate for the exact list. */}
        <p className="muted note">
          Pausing stops new calls only. A command already running and a stream already open keep
          going until they finish.
        </p>
        {switchProblem && <p className="problem">{switchProblem}</p>}
      </section>

      <section>
        <h2>What this computer offers</h2>
        {announcement ? (
          <>
            <ul className="caps">
              {announcement.capabilities.map((capability) => (
                <li key={capability.name}>
                  <p className="cap-head">
                    <span className="mono">{capability.name}</span>{" "}
                    <span className="muted">
                      version {capability.version} · {toolCount(capability.tools.length)}
                    </span>
                  </p>
                  <p className="mono cap-tools">{capability.tools.join(", ")}</p>
                </li>
              ))}
            </ul>
            <p className="muted note">
              A path an agent sends without a leading slash starts in{" "}
              <span className="mono">{announcement.root}</span>. That is where relative paths
              start, not a boundary: an absolute path goes wherever it names, and a command can
              work anywhere you can.
            </p>
          </>
        ) : (
          <p className={announcementProblem ? "problem" : "muted"}>
            {announcementProblem ?? "Reading what this computer offers."}
          </p>
        )}
      </section>

      <section>
        <h2>What has arrived</h2>
        {inboxProblem ? (
          // Never the "nothing yet" line on a failed read. The inbox is the one place on this
          // machine a stranger's file is written to, and telling somebody it is empty because a
          // directory would not open is the same confident false negative the audit tail shipped.
          <p className="problem">{inboxProblem}</p>
        ) : inbox === undefined ? (
          <p className="muted">Reading what has arrived.</p>
        ) : inbox === null ? (
          <p className="muted">
            File transfer is not running on this computer, so nothing can arrive here and there is
            no inbox to read. Zyris says why in its log when it starts.
          </p>
        ) : (
          <>
            {inbox.length === 0 ? (
              <p className="muted">Nothing has arrived yet.</p>
            ) : (
              <>
                <ol className="calls">
                  {inbox.map((entry) => {
                    const when = arrived(entry.received_unix_ms);
                    return (
                      <li className="call" key={entry.path}>
                        <div className="call-head">
                          {/* Both names are text this program did not choose — the folder is
                              named after the machine Attacca says sent the file, the file after
                              whatever the sender called it — so both are set in a monospace face
                              and only the word between them is the screen's own. A file named
                              "from your laptop, approved" must not read as this row's wording. */}
                          <span className="inbox-what" title={`${entry.name} from ${entry.from}`}>
                            <span className="mono">{entry.name}</span>{" "}
                            <span className="muted">
                              from <span className="mono">{entry.from}</span>
                            </span>
                          </span>
                          <span className="call-when">
                            <span className="muted" title={`${entry.bytes} bytes`}>
                              {fileSize(entry.bytes)}
                            </span>
                            {when ? (
                              <time className="muted" dateTime={when.iso} title={when.iso}>
                                {when.label}
                              </time>
                            ) : (
                              // Zero is what upstream reports when the filesystem will not give
                              // a modification time. Saying so beats dating the file to 1970.
                              <span className="muted">time unknown</span>
                            )}
                          </span>
                        </div>
                        {/* Wraps and then stops at two lines, with the whole path on the row —
                            the same rule the audit rows follow, and the reason nothing on this
                            screen scrolls sideways. */}
                        <p className="mono call-detail" title={entry.path}>
                          {entry.path}
                        </p>
                      </li>
                    );
                  })}
                </ol>
                <p className="muted note">
                  Showing {inbox.length === 1 ? "1 file" : `${inbox.length} files`}, newest first.
                  The time is when the file was last written here, in this computer's local time —
                  which is when it arrived, unless something has changed it since.
                </p>
              </>
            )}
            {/* The section sits under a pause switch that does not stop an arriving file and
                beside an approval that does not gate one. Both are easy to read as covering this
                list, and neither does: the confirmer is consulted on `send_to` and nowhere else,
                and a delivery never asks this machine's agent surface for anything, so the gate
                never sees it. Three times this project has shipped copy claiming more than the
                code does; this paragraph is where the fourth would go. */}
            <p className="muted note">
              Any machine enrolled on your Attacca account can send files here, whether or not you
              have approved it. Approving a machine decides what this computer will send to it, not
              what arrives from it — and pausing does not stop a file arriving either, because
              nothing an agent asks of this computer is involved in one.
            </p>
          </>
        )}
      </section>

      <section>
        <h2>What ran</h2>
        {callsProblem ? (
          // Never the "nothing yet" line on a failed read: that is a confident false negative
          // about the one record of what touched this machine. The live rows above are still
          // shown — they are what this window heard directly.
          <p className="problem">{callsProblem}</p>
        ) : null}
        {rows.length === 0 && !callsProblem ? (
          <p className="muted">No agent has run anything on this computer yet.</p>
        ) : rows.length === 0 ? null : (
          <>
            <ol className="calls">
              {rows.map((row, index) => (
                <li className="call" key={`${row.at}-${index}`}>
                  <div className="call-head">
                    <span className="mono call-what">
                      {row.capability} · {row.tool}
                    </span>
                    <span className="call-when">
                      <time className="muted" dateTime={row.at} title={row.at}>
                        {clockTime(row.at)}
                      </time>
                      <span className={`badge badge-${row.outcome}`}>{row.outcome}</span>
                    </span>
                  </div>
                  {/* A command line or a path will be longer than the row. It wraps and then
                      stops at two lines, with the whole value on the row itself; nothing on
                      this screen ever scrolls sideways. */}
                  {row.detail && (
                    <p className="mono call-detail" title={row.detail}>
                      {row.detail}
                    </p>
                  )}
                </li>
              ))}
            </ol>
            <p className="muted note">
              Showing {rows.length} of the last {MAX_TOOL_CALLS} this window keeps, newest first,
              in this computer's local time.
              {announcement && (
                <>
                  {" "}
                  The whole record is <span className="mono">{announcement.auditLog}</span>.
                </>
              )}
            </p>
          </>
        )}
      </section>
    </main>
  );
}
