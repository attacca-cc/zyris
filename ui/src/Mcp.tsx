import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { State } from "./state";

// One tool a server offered that this machine did not announce, and why. `zyris_mcp::DroppedTool`,
// serialized with both field names as written.
type DroppedTool = {
  name: string;
  reason: string;
};

// What one configured server is doing. `zyris_tools::ServerState`, internally tagged on `state` —
// which is also the name of the field holding it, so the switch below reads `server.state.state`.
// A test in crates/zyris-tools/src/servers.rs pins that nesting; nothing checks it at build time.
//
// **Four states and not two.** `disabled` and `died` both end as "this capability is not
// announced", and to an agent that is the whole truth. To whoever is looking at this screen it is
// not: one of them is a switch they moved and the other is a process that fell over.
type ServerState =
  | { state: "running" }
  | { state: "disabled" }
  | { state: "died" }
  | { state: "failed"; reason: string };

// One configured server, as `zyris_tools::ServerView` serializes it.
type ServerView = {
  name: string;
  // The capability an agent addresses, or null for a name that could never make one — see
  // `zyris_mcp::capability_name`. Null is a thing to fix, not a thing to hide.
  capability: string | null;
  command: string;
  args: string[];
  state: ServerState;
  // What is announced right now. Empty whenever the server is not running, which is honest:
  // nothing of its is announced then.
  tools: string[];
  dropped: DroppedTool[];
};

// What `mcp_servers` answers with: `bridge::ServerList`.
//
// **Three answers live in here and only one of them is a list.** `problem` set is a server list
// that could not be read; `problem` clear with no servers is a machine nobody has configured. The
// core keeps those apart (`zyris_mcp::Started`) precisely so this screen does not have to guess,
// and rendering an unreadable file as "you have configured nothing" is the confident false
// negative the audit tail and the inbox each shipped once.
type ServerList = {
  path: string;
  problem: string | null;
  servers: ServerView[];
};

// A rejected `invoke` carries whatever the command returned as its error. These commands return
// strings, so anything else means the bridge itself broke and is not worth showing verbatim.
function asMessage(error: unknown, fallback: string): string {
  return typeof error === "string" ? error : fallback;
}

function toolCount(count: number): string {
  return count === 1 ? "1 tool" : `${count} tools`;
}

// The short word on the row. Short because `.badge` does not wrap: a long one would be the only
// thing on this screen that could push it sideways.
//
// The sentence under the row carries the meaning; this is only the thing the eye lands on first,
// and its job is that a reader scanning a list of eight servers can see which one is wrong.
function badge(state: ServerState): { label: string; className: string } {
  switch (state.state) {
    case "running":
      return { label: "running", className: "badge-on" };
    case "disabled":
      return { label: "turned off", className: "badge-off" };
    case "died":
      return { label: "stopped", className: "badge-down" };
    case "failed":
      return { label: "did not start", className: "badge-down" };
  }
}

// What is actually run, for somebody who wants to try it in a terminal.
//
// Joined with spaces because that is how a person reads a command, and the arguments are shown
// exactly as the file has them — nothing here goes through a shell, so an argument with a space in
// it looks like two and would need quoting to run by hand. The note under the list says so.
function commandLine(server: ServerView): string {
  return [server.command, ...server.args].join(" ");
}

export function Mcp({ state }: { state: State }) {
  // The list, or `undefined` while the first read is in flight. Never `[]` for a read that has not
  // answered, and never `[]` for one that failed: see `problem`.
  const [list, setList] = useState<ServerList | undefined>(undefined);
  // A read that did not come back at all, kept apart from `list` for the reason Tools.tsx keeps
  // `inboxProblem` apart from `inbox`. A failed read is not an empty list.
  const [problem, setProblem] = useState<string | null>(null);
  // The server whose switch is in flight. Starting one can take as long as
  // `zyris_mcp::STARTUP_DEADLINE` — ten seconds for a command that never speaks — and the whole
  // list waits behind the same lock, so a switch that looked instant would be the screen lying
  // about what it is doing.
  const [moving, setMoving] = useState<string | null>(null);
  // Why one server's switch would not move, by server name. Per row rather than one message for
  // the screen: "it would not start" belongs beside the server it is about.
  const [refused, setRefused] = useState<Record<string, string>>({});

  // Re-read whenever the core says a server changed, and once on the way in.
  //
  // **The whole list, not the row the event named.** The supervisor is the authority on what is
  // announced — it is the same one the node builds from — so a screen that applied events to its
  // own copy would be a second opinion, wrong for good the first time an event was dropped for a
  // window that had fallen behind. The event's job is only to say that something moved.
  //
  // The case that makes it necessary is the one nobody clicked: a server's process falls over, the
  // core notices within `zyris_tools::HEALTH_INTERVAL` and withdraws it. Without this the row goes
  // on saying "running" until somebody reopens the window.
  useEffect(() => {
    // The same guard App.tsx's catch-up uses: StrictMode runs this effect twice, and without it
    // the first run's answer can land on top of the second's.
    let cancelled = false;

    void invoke<ServerList>("mcp_servers")
      .then((answer) => {
        if (cancelled) return;
        setList(answer);
        // An answer is an answer: a message from a read that failed earlier has stopped being
        // true, and leaving it above a fresh list reads as a broken screen.
        setProblem(null);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        // Deliberately not `setList({ ...empty })`. Whatever was on the screen is still the last
        // thing this machine said about its servers, and an empty list here would claim there are
        // none.
        setProblem(asMessage(error, "Could not read the MCP server list."));
      });

    return () => {
      cancelled = true;
    };
    // `state.resyncs` is the other half, and it covers the case `mcpChange` cannot: a window that
    // fell behind on the event bus was never told which server moved, because a server change is
    // published transiently and nothing keeps the last one. Without it such a window goes on
    // showing a dead server as running.
  }, [state.mcpChange, state.resyncs]);

  function toggle(server: ServerView) {
    const enabled = server.state.state !== "running";
    setMoving(server.name);
    setRefused((all) => {
      const { [server.name]: _gone, ...rest } = all;
      return rest;
    });

    void invoke<ServerView>("set_mcp_server_enabled", { name: server.name, enabled })
      .then((answer) => {
        // **What the command said, not what the click asked for.** Turning a server on is the case
        // that proves the difference: the command may not be there any more, and the honest answer
        // is a failure carrying the reason rather than the "running" that was requested.
        setList((current) =>
          current
            ? {
                ...current,
                servers: current.servers.map((one) => (one.name === answer.name ? answer : one)),
              }
            : current,
        );
      })
      .catch((error: unknown) => {
        setRefused((all) => ({
          ...all,
          [server.name]: asMessage(error, "That switch would not move."),
        }));
      })
      .finally(() => setMoving(null));
  }

  return (
    <main className="screen screen-wide">
      <h1>MCP</h1>
      <p className="lead">
        The MCP servers this computer is configured to run, and the tools each one adds to what
        your agents can call here.
      </p>

      {problem && <p className="problem">{problem}</p>}

      {list === undefined ? (
        problem ? null : (
          <p className="muted">Reading the server list.</p>
        )
      ) : list.problem ? (
        // Never the "nothing configured" line on a failed read. A file with a typo in it starts no
        // servers, exactly as a machine nobody has configured does, and telling the person who
        // wrote that file they have configured nothing sends them looking for the file they are
        // already looking at.
        <section>
          <p className="problem">
            The server list could not be read, so none of the servers in it were started. Nothing
            else on this computer is affected. {list.problem}
          </p>
          <p className="muted note">
            The file is <span className="mono">{list.path}</span>. Zyris reads it when it starts,
            so fix it and restart Zyris.
          </p>
        </section>
      ) : list.servers.length === 0 ? (
        <section>
          <p className="muted">No MCP servers are configured on this computer.</p>
          <p className="muted note">
            Zyris looks for them in <span className="mono">{list.path}</span>, which does not have
            to exist. An entry names the server, the command to run and the arguments to pass it,
            and may add <span className="mono">"enabled": false</span> to be listed here without
            being started:
          </p>
          <pre className="snippet">{EXAMPLE}</pre>
          <p className="muted note">
            Its tools are then announced as one capability called{" "}
            <span className="mono">mcp_desk-notes</span> — <span className="mono">mcp_</span> and
            the name you gave it. A name with a dot in it cannot be announced, and two entries
            sharing a name stop every server in the file from starting. Zyris reads this file when
            it starts, so restart it after an edit.
          </p>
        </section>
      ) : (
        <section>
          <ul className="caps">
            {list.servers.map((server) => {
              const { label, className } = badge(server.state);
              const inFlight = moving === server.name;
              return (
                <li key={server.name} aria-label={server.name}>
                  <div className="call-head">
                    <span className="mono call-what">{server.name}</span>
                    <span className="call-when">
                      <span className={`badge ${className}`}>{label}</span>
                      {/* No switch for a server that can never be announced: starting it fails for
                          the same reason every time, and the fix is the rename below. */}
                      {server.capability !== null && (
                        <button
                          type="button"
                          className="button button-quiet"
                          disabled={inFlight}
                          onClick={() => toggle(server)}
                        >
                          {server.state.state === "running"
                            ? inFlight
                              ? "Turning off"
                              : "Turn off"
                            : inFlight
                              ? "Turning on"
                              : "Turn on"}
                        </button>
                      )}
                    </span>
                  </div>

                  <p className="mono call-detail" title={commandLine(server)}>
                    {commandLine(server)}
                  </p>

                  {server.state.state === "running" && (
                    <>
                      {/* Not "…that an agent can call": with the pause switch on it cannot, and
                          a sentence that has to be re-read against another screen to be true is
                          one this project has shipped three times too often. */}
                      <p className="note muted">
                        Announced as <span className="mono">{server.capability}</span>, with{" "}
                        {toolCount(server.tools.length)}.
                      </p>
                      {server.tools.length > 0 && (
                        <p className="mono cap-tools">{server.tools.join(", ")}</p>
                      )}
                    </>
                  )}

                  {server.state.state === "disabled" && (
                    <p className="note muted">
                      Turned off. It is not running, and nothing of its is announced.
                    </p>
                  )}

                  {server.state.state === "died" && (
                    // The distinction the core carries all the way here, and the last place it
                    // could be thrown away. Both are "not announced"; only this one is a process
                    // to restart, and nobody chose it.
                    <p className="note problem">
                      Its process stopped on its own — nobody asked for that — so its tools are no
                      longer announced. Turn it on again to start it afresh.
                    </p>
                  )}

                  {/* The reason on its own. The badge beside the name already reads "did not
                      start", and this used to be rendered behind a second "It did not start." —
                      so a row whose reason began the same way said it twice. `zyris_tools`'s
                      `startup_failure` is where the wording lives now, and it is written to be
                      the whole of what the row says. */}
                  {server.state.state === "failed" && (
                    <p className="note problem">{server.state.reason}</p>
                  )}

                  {server.capability === null && (
                    // `capability_name` refuses exactly two names, and both are fixed the same
                    // way. Said as the action rather than as the rule.
                    <p className="note problem">
                      This server's name cannot be announced: a capability name cannot be empty and
                      cannot contain a dot, because an agent addresses a tool as{" "}
                      <span className="mono">capability.tool</span>. Rename it in the server list
                      and restart Zyris.
                    </p>
                  )}

                  {server.dropped.length > 0 && (
                    // An agent sees a tool list, never a list of absences, so it cannot tell a
                    // tool that was dropped from one this server never had. This is the only place
                    // anybody could.
                    <ul className="dropped">
                      {server.dropped.map((tool) => (
                        <li key={tool.name} className="note muted">
                          <span className="mono">{tool.name}</span> is not announced: {tool.reason}
                        </li>
                      ))}
                    </ul>
                  )}

                  {refused[server.name] && <p className="note problem">{refused[server.name]}</p>}
                </li>
              );
            })}
          </ul>

          {/* The claim that would be easiest to get wrong, and the one this project has got wrong
              four times. `guarded.rs` switches argument summarising off for every capability named
              `mcp_*`: the allowlist it would otherwise use matches on spelling, and a third party's
              `path` or `command` shares the spelling with this machine's own and none of the
              meaning — it could as easily be a password. The three words are `Outcome`'s three and
              nothing more: `allowed` there means the call was accepted, which for an MCP tool says
              nothing about whether the server was happy with it.

              The second sentence is the fifth thing this copy got wrong, and it was found by
              reading `Guarded::dispatch` rather than by running anything: the line is written
              *when the call finishes*, and a call that is cut off before it finishes — an agent
              that gave up on a slow server, a connection that dropped — is carried by a task
              `zyris-core` aborts, which never reaches the line. Saying "the log records that an
              MCP tool was called" claimed more than that. */}
          <p className="muted note">
            A promoted tool goes through the same pause switch and the same audit log as this
            computer's own. When a call finishes, the log records that an MCP tool was called —
            when, which server, which tool, and whether the call was allowed, refused or failed —
            and not what was asked of it. Nothing an agent sends to one of these servers is
            written down.
          </p>
          <p className="muted note">
            A call that never finishes is not written down either. These servers are given no time
            limit, so one that accepts a request and goes quiet waits until the agent gives up or
            the connection drops — and a call cut off that way leaves no line at all. The same is
            true of this computer's own <span className="mono">terminal.exec</span> with no
            timeout.
          </p>

          {/* The switch is a real switch and it really is only for this run. A toggle that forgot
              silently would be a screen that lied, so this is the sentence that makes not writing
              the file a decision rather than a gap. `zyris_tools::Servers::set_enabled` has the
              three reasons. */}
          <p className="muted note">
            Turning a server off here stops its process and takes its tools off what this computer
            announces, straight away. It lasts until Zyris restarts, and so does turning one on:
            the server list decides what starts, and Zyris never writes to it. To keep a server
            off, or to add or remove one, edit <span className="mono">{list.path}</span> — an entry
            with <span className="mono">"enabled": false</span> is listed here and not started.
            Zyris reads that file when it starts, so restart it after an edit.
          </p>

          <p className="muted note">
            Each server is run directly, with its arguments passed exactly as the file has them:
            nothing goes through a shell, so a command shown above may need quoting to run by hand.
          </p>
        </section>
      )}
    </main>
  );
}

// What an entry looks like, for a machine that has none. Two spaces and real values rather than
// placeholders in angle brackets: this is meant to be copied and edited, and a person who has
// never seen the file should be able to tell which parts are theirs.
const EXAMPLE = `{
  "servers": [
    { "name": "desk-notes", "command": "notes-mcp", "args": ["--root", "/home/you/notes"] }
  ]
}`;
