import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { MAX_TOOL_CALLS, type Action, type State, type ToolCallRow } from "./state";

// What the `announced_tools` command answers with: `zyris_tools::Announcement`, serialized
// camelCase. A test in crates/zyris-tools/src/announce.rs pins those field names; this is the
// other half of that agreement, and nothing checks the two at build time.
type Announcement = {
  capabilities: { name: string; version: number; tools: string[] }[];
  root: string;
  auditLog: string;
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

export function Tools({ state, dispatch }: { state: State; dispatch: (action: Action) => void }) {
  const [announcement, setAnnouncement] = useState<Announcement | null>(null);
  const [announcementProblem, setAnnouncementProblem] = useState<string | null>(null);
  const [stored, setStored] = useState<ToolCallRow[]>([]);
  const [callsProblem, setCallsProblem] = useState<string | null>(null);
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
    };
    // Mount only, deliberately. The stored tail is a snapshot to sit beneath the live calls, and
    // re-reading it whenever a call arrived would move the boundary out from under those calls.
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
        What your Attacca agents can reach on this computer, and what they have run.
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
