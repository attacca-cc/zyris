import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { State } from "./state";
import { ChevronDownIcon, InfoIcon, ServerIcon, TriangleAlertIcon } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { IconTile, Mono, Note, Problem } from "@/components/IconTile";
import { Page, PageHeader } from "@/components/PageHeader";
import { cn } from "@/lib/utils";

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
function badge(state: ServerState): { label: string; variant: "success" | "secondary" | "destructive" } {
  switch (state.state) {
    case "running":
      return { label: "running", variant: "success" };
    case "disabled":
      return { label: "turned off", variant: "secondary" };
    case "died":
      return { label: "stopped", variant: "destructive" };
    case "failed":
      return { label: "did not start", variant: "destructive" };
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

// What to call a row on the screen.
//
// `{ "name": "", "command": "x" }` is a server list this code accepts — `name` is required and an
// empty string is a string — and such an entry can never be announced, so it is listed here as
// something to fix like any other. Rendered as its own name it is a row with a blank heading, an
// empty `aria-label` and an empty React key, identifiable only by the command underneath it. Two
// of them cannot happen: the server list refuses two entries sharing a name, and "" is a name.
function rowName(server: ServerView): string {
  return server.name === "" ? "(this entry has no name)" : server.name;
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
    <Page wide>
      <PageHeader
        title="MCP servers"
        description="Extra tools from MCP servers on this computer. Your agents can call them like any other tool."
      />

      {problem && <Problem>{problem}</Problem>}

      {list === undefined ? (
        problem ? null : <Note>Reading the server list.</Note>
      ) : list.problem ? (
        <Card>
          <Problem>
            The server list could not be read, so none of the servers in it were started. Nothing else on
            this computer is affected. {list.problem}
          </Problem>
          <Note>
            The file is <Mono>{list.path}</Mono>. Zyris reads it when it starts, so fix it and restart
            Zyris.
          </Note>
        </Card>
      ) : list.servers.length === 0 ? (
        <Card className="items-start">
          <CardHeader className="items-center">
            <IconTile tone="muted">
              <ServerIcon />
            </IconTile>
            <div className="flex flex-col gap-0.5">
              <CardTitle>No MCP servers are configured on this computer</CardTitle>
              <CardDescription>
                Add one to <Mono>{list.path}</Mono> and restart Zyris. An entry names the server, the
                command to run and its arguments:
              </CardDescription>
            </div>
          </CardHeader>
          <pre className="m-0 w-full overflow-x-auto rounded-lg border bg-inset px-4 py-3 font-mono text-xs text-heading">
            {EXAMPLE}
          </pre>
          <Note>
            Its tools are announced as one capability called <Mono>mcp_desk-notes</Mono> — <Mono>mcp_</Mono>{" "}
            and the name you gave it. Add <Mono>"enabled": false</Mono> to list a server here without
            starting it. A name with a dot in it cannot be announced.
          </Note>
        </Card>
      ) : (
        <>
          <ul className="m-0 flex list-none flex-col gap-3 p-0">
            {list.servers.map((server) => {
              const { label, variant } = badge(server.state);
              const inFlight = moving === server.name;
              const running = server.state.state === "running";
              const broken = server.state.state === "died" || server.state.state === "failed";
              return (
                <li key={rowName(server)} aria-label={rowName(server)}>
                  <Card className="gap-3 py-4.5">
                    <CardHeader className="items-center">
                      <IconTile tone={broken ? "destructive" : running ? "accent" : "muted"}>
                        {broken ? <TriangleAlertIcon /> : <ServerIcon />}
                      </IconTile>
                      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
                        <CardTitle className={cn(!running && !broken && "text-[#bdb5ad]")}>{rowName(server)}</CardTitle>
                        <span className="truncate font-mono text-xs text-muted-foreground" title={commandLine(server)}>
                          {commandLine(server)}
                        </span>
                      </div>
                      <div className="flex items-center gap-3">
                        <Badge variant={variant}>{label}</Badge>
                        {/* No switch for a server that can never be announced: starting it fails for
                            the same reason every time, and the fix is the rename below. */}
                        {server.capability !== null && (
                          <Switch
                            aria-label={`Turn ${rowName(server)} ${running ? "off" : "on"}`}
                            checked={running}
                            disabled={inFlight}
                            onCheckedChange={() => toggle(server)}
                          />
                        )}
                      </div>
                    </CardHeader>

                    <div className="flex flex-col gap-2 pl-12">
                      {running && (
                        <>
                          {server.tools.length > 0 && (
                            <div className="flex flex-wrap gap-1.5">
                              {server.tools.map((tool) => (
                                <span key={tool} className="rounded-md border bg-muted px-2 py-0.5 font-mono text-xs text-foreground">
                                  {tool}
                                </span>
                              ))}
                            </div>
                          )}
                          <Note className="text-xs">
                            Announced as <Mono className="text-xs">{server.capability}</Mono>, with{" "}
                            {toolCount(server.tools.length)}.
                          </Note>
                        </>
                      )}
                      {server.state.state === "disabled" && (
                        <Note className="text-xs">Turned off. It is not running, and nothing of its is announced.</Note>
                      )}
                      {server.state.state === "died" && (
                        <Problem>
                          Its process stopped on its own — nobody asked for that — so its tools are no longer
                          announced. Turn it on again to start it afresh.
                        </Problem>
                      )}
                      {/* The reason on its own: the badge already says it did not start. */}
                      {server.state.state === "failed" && <Problem>{server.state.reason}</Problem>}
                      {server.capability === null && (
                        <Problem>
                          This server's name cannot be announced: a capability name cannot be empty or
                          contain a dot, because an agent addresses a tool as <Mono>capability.tool</Mono>.
                          Rename it in the server list and restart Zyris.
                        </Problem>
                      )}
                      {server.dropped.length > 0 && (
                        <ul className="m-0 flex list-none flex-col gap-1 p-0">
                          {server.dropped.map((tool) => (
                            <li key={tool.name} className="text-xs text-muted-foreground">
                              <Mono className="text-xs">{tool.name}</Mono> is not announced: {tool.reason}
                            </li>
                          ))}
                        </ul>
                      )}
                      {refused[server.name] && <Problem>{refused[server.name]}</Problem>}
                    </div>
                  </Card>
                </li>
              );
            })}
          </ul>

          <details className="group rounded-lg border border-sidebar-border px-4 py-3 text-[0.8125rem] text-muted-foreground">
            <summary className="flex cursor-pointer list-none items-center gap-2 text-foreground">
              <InfoIcon className="size-4 text-muted-foreground" aria-hidden="true" />
              How these servers are run and recorded
              <ChevronDownIcon className="ml-auto size-4 transition-transform group-open:rotate-180" aria-hidden="true" />
            </summary>
            <div className="mt-3 flex flex-col gap-2">
              {/* The claims easiest to get wrong, and the ones this project has got wrong most. */}
              <p className="m-0">
                A promoted tool goes through the same pause switch and audit log as this computer's own.
                When a call finishes, the log records that an MCP tool was called — when, which server,
                which tool, and whether it was allowed, refused or failed — and not what was asked of it.
              </p>
              <p className="m-0">
                A call that never finishes is not written down either. These servers are given no time
                limit, so one that goes quiet waits until the agent gives up or the connection drops.
              </p>
              <p className="m-0">
                A switch moved here lasts until Zyris restarts. To keep a server off, or to add or remove
                one, edit <Mono>{list.path}</Mono> — an entry with <Mono>"enabled": false</Mono> is listed
                and not started.
              </p>
              <p className="m-0">
                Each server is run directly, with its arguments exactly as the file has them: nothing goes
                through a shell.
              </p>
            </div>
          </details>
        </>
      )}
    </Page>
  );
}

const EXAMPLE = `{
  "servers": [
    { "name": "desk-notes", "command": "notes-mcp", "args": ["--root", "/home/you/notes"] }
  ]
}`;
