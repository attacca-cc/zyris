import { useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  FileIcon,
  HistoryIcon,
  InboxIcon,
  PauseIcon,
  PlayIcon,
  ShieldCheckIcon,
  ShieldOffIcon,
  TerminalIcon,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { IconTile, Mono, Note, Problem } from "@/components/IconTile";
import { Page, PageHeader } from "@/components/PageHeader";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { MAX_TOOL_CALLS, type Action, type State, type ToolCallRow } from "./state";

// What the `announced_tools` command answers with: `zyris_tools::Announcement`, serialized
// camelCase. A test in crates/zyris-tools/src/announce.rs pins those field names; this is the
// other half of that agreement, and nothing checks the two at build time.
//
// **What it lists is what the node is announcing at the moment it is asked**, promoted MCP
// servers included — see the effect that reads it for why this screen has to ask more than once.
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

  // What this computer announces, read again whenever the core says a local MCP server moved.
  //
  // **An effect of its own, and not part of the mount-only one above.** What is announced is no
  // longer fixed for the run: a promoted MCP server is withdrawn when somebody turns it off and
  // when its process falls over, and joins when somebody turns it back on. A list read once would
  // go on telling a person that agents can reach a capability this node has withdrawn — and hide
  // one it has added — which is a screen stating something false about what this machine hands
  // out. The core has no snapshot to be stale any more (`zyris_tools::Tools::announcement` reads
  // the node's own list), so all that is left is asking again.
  //
  // `state.mcpChange` is the signal, and it is the same one the MCP screen uses. The five
  // built-ins never move, so a change to one of these servers is the only thing that can change
  // this answer.
  useEffect(() => {
    let cancelled = false;

    void invoke<Announcement>("announced_tools")
      .then((answer) => {
        if (cancelled) return;
        setAnnouncement(answer);
        // An answer is an answer: a message from a read that failed earlier has stopped being
        // true, and leaving it above a fresh list reads as a broken screen.
        setAnnouncementProblem(null);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        // Deliberately not an empty announcement. Whatever is on the screen is still the last
        // thing this computer said about itself, and a blank list here would claim it offers an
        // agent nothing at all.
        setAnnouncementProblem(asMessage(error, "Could not read what this computer offers."));
      });

    return () => {
      cancelled = true;
    };
    // And `state.resyncs` for the window that fell behind on the bus and was never told which
    // server moved — see the MCP screen's copy of this effect and `RESYNC_EVENT_NAME`.
  }, [state.mcpChange, state.resyncs]);

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

  const activity = (
    <>
      {callsProblem && <Problem>{callsProblem}</Problem>}
      {rows.length === 0 && !callsProblem ? (
        <Empty icon={<HistoryIcon />}>No agent has run anything on this computer yet.</Empty>
      ) : rows.length === 0 ? null : (
        <Card className="gap-0 overflow-hidden p-0">
          <div className="grid grid-cols-[4.5rem_minmax(0,12rem)_minmax(0,1fr)_5.5rem] gap-4 border-b px-5 py-2.5 text-xs text-subtle max-[820px]:grid-cols-[4.5rem_minmax(0,1fr)_5.5rem]">
            <span>Time</span>
            <span>Tool</span>
            <span className="max-[820px]:hidden">Details</span>
            <span className="text-right">Result</span>
          </div>
          <ol className="m-0 list-none p-0">
            {rows.map((row, index) => (
              <li
                key={`${row.at}-${index}`}
                className="grid grid-cols-[4.5rem_minmax(0,12rem)_minmax(0,1fr)_5.5rem] items-center gap-4 border-b border-[#1d1814] px-5 py-3 text-[0.8125rem] last:border-b-0 max-[820px]:grid-cols-[4.5rem_minmax(0,1fr)_5.5rem]"
              >
                <time className="text-muted-foreground tabular-nums" dateTime={row.at} title={row.at}>
                  {clockTime(row.at)}
                </time>
                <span className="flex min-w-0 items-center gap-2 text-heading">
                  <span aria-hidden="true" className="size-1.5 shrink-0 rounded-[2px] bg-primary" />
                  <span className="truncate font-mono text-xs">
                    {row.capability} · {row.tool}
                  </span>
                </span>
                {/* A command line or a path will be longer than the row: it is cut to one line
                    here, with the whole value on hover. Nothing on this screen scrolls sideways. */}
                <span className="truncate font-mono text-xs text-muted-foreground max-[820px]:hidden" title={row.detail}>
                  {row.detail}
                </span>
                <span className="text-right">
                  <Badge variant={OUTCOME[row.outcome] ?? "secondary"}>{row.outcome}</Badge>
                </span>
              </li>
            ))}
          </ol>
          <div className="flex items-center justify-between gap-4 border-t px-5 py-3 text-xs text-subtle">
            <span>
              Showing {rows.length} of the last {MAX_TOOL_CALLS} this window keeps, newest first.
            </span>
            {announcement && (
              <span className="truncate" title={announcement.auditLog}>
                Full record: <Mono className="text-xs">{announcement.auditLog}</Mono>
              </span>
            )}
          </div>
        </Card>
      )}
    </>
  );

  const offers = (
    <>
      {announcement ? (
        <>
          <Card className="gap-0 overflow-hidden p-0">
            <ul className="m-0 list-none p-0">
              {announcement.capabilities.map((capability) => (
                <li key={capability.name} className="flex flex-col gap-2 border-b border-[#1d1814] px-5 py-3.5 last:border-b-0">
                  <div className="flex items-center gap-2.5">
                    <TerminalIcon className="size-4 text-primary" aria-hidden="true" />
                    <span className="font-mono text-[0.8125rem] text-heading">{capability.name}</span>
                    <span className="text-xs text-subtle">
                      version {capability.version} · {toolCount(capability.tools.length)}
                    </span>
                  </div>
                  <div className="flex flex-wrap gap-1.5 pl-6.5">
                    {capability.tools.map((tool) => (
                      <span key={tool} className="rounded-md border bg-muted px-2 py-0.5 font-mono text-xs text-foreground">
                        {tool}
                      </span>
                    ))}
                  </div>
                </li>
              ))}
            </ul>
          </Card>
          <Note>
            A path an agent sends without a leading slash starts in <Mono>{announcement.root}</Mono>. That
            is where relative paths start, not a boundary: an absolute path goes wherever it names.
          </Note>
        </>
      ) : (
        !announcementProblem && <Note>Reading what this computer offers.</Note>
      )}
      {/* Shown whether or not there is a list: a read failing after one succeeded leaves the last
          answer on screen, and saying nothing would be a correct-looking list gone stale. */}
      {announcementProblem && (
        <Problem>
          {announcementProblem}
          {announcement && " What is listed above is the last answer this computer gave."}
        </Problem>
      )}
    </>
  );

  const received = (
    <>
      {inboxProblem ? (
        <Problem>{inboxProblem}</Problem>
      ) : inbox === undefined ? (
        <Note>Reading what has arrived.</Note>
      ) : inbox === null ? (
        <Empty icon={<InboxIcon />}>
          File transfer is not running on this computer, so nothing can arrive here. Zyris says why in its
          log when it starts.
        </Empty>
      ) : inbox.length === 0 ? (
        <Empty icon={<InboxIcon />}>Nothing has arrived yet.</Empty>
      ) : (
        <Card className="gap-0 overflow-hidden p-0">
          <ol className="m-0 list-none p-0">
            {inbox.map((entry) => {
              const when = arrived(entry.received_unix_ms);
              return (
                <li key={entry.path} className="flex items-center gap-3 border-b border-[#1d1814] px-5 py-3 last:border-b-0">
                  <FileIcon className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
                  {/* Both names are text this program did not choose, so both are monospace and
                      only the word between them is the screen's own. */}
                  <div className="flex min-w-0 flex-1 flex-col gap-0.5" title={entry.path}>
                    <span className="truncate text-[0.8125rem] text-heading">
                      <Mono>{entry.name}</Mono>{" "}
                      <span className="text-muted-foreground">
                        from <Mono>{entry.from}</Mono>
                      </span>
                    </span>
                    <span className="truncate font-mono text-xs text-subtle">{entry.path}</span>
                  </div>
                  <span className="shrink-0 text-xs text-muted-foreground" title={`${entry.bytes} bytes`}>
                    {fileSize(entry.bytes)}
                  </span>
                  {when ? (
                    <time className="w-28 shrink-0 text-right text-xs text-muted-foreground" dateTime={when.iso} title={when.iso}>
                      {when.label}
                    </time>
                  ) : (
                    <span className="w-28 shrink-0 text-right text-xs text-muted-foreground">time unknown</span>
                  )}
                </li>
              );
            })}
          </ol>
        </Card>
      )}
      {inbox !== undefined && inbox !== null && !inboxProblem && (
        // Easy to read as covered by the pause switch or by approving a machine, and neither
        // does: this is where claiming more than the code does would go.
        <Note>
          Any machine on your Attacca account can send files here, whether or not you approved it, and
          pausing does not stop a file arriving.
        </Note>
      )}
    </>
  );

  return (
    <Page wide>
      <PageHeader title="Tools" description="What your agents can do on this computer, and what they have done." />

      <Card>
        <CardHeader className="items-center">
          <IconTile tone={state.paused ? "muted" : "accent"}>
            {state.paused ? <ShieldOffIcon /> : <ShieldCheckIcon />}
          </IconTile>
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <CardTitle>{state.paused ? "Paused" : "Agents can use this computer"}</CardTitle>
            <CardDescription>
              {state.paused
                ? "No new commands or file access will be accepted."
                : "They run commands and read and write files as you."}{" "}
              {/* The switch stops calls arriving; it does not reach into one already under way. */}
              Pausing stops new calls only; anything already running finishes.
            </CardDescription>
          </div>
          <Button variant={state.paused ? "default" : "outline"} onClick={toggle}>
            {state.paused ? <PlayIcon /> : <PauseIcon />}
            {state.paused ? "Resume" : "Pause"}
          </Button>
        </CardHeader>
        {switchProblem && <Problem>{switchProblem}</Problem>}
      </Card>

      <Tabs defaultValue="activity">
        <TabsList>
          <TabsTrigger value="activity">
            <HistoryIcon aria-hidden="true" />
            Activity
          </TabsTrigger>
          <TabsTrigger value="offers">
            <TerminalIcon aria-hidden="true" />
            Capabilities
            {announcement && <Count>{announcement.capabilities.length}</Count>}
          </TabsTrigger>
          <TabsTrigger value="received">
            <InboxIcon aria-hidden="true" />
            Received files
            {inbox && inbox.length > 0 && <Count>{inbox.length}</Count>}
          </TabsTrigger>
        </TabsList>
        {/* All three stay mounted, so each is read once and switching is instant. */}
        <TabsContent value="activity" forceMount className="flex flex-col gap-3 data-[state=inactive]:hidden">
          {activity}
        </TabsContent>
        <TabsContent value="offers" forceMount className="flex flex-col gap-3 data-[state=inactive]:hidden">
          {offers}
        </TabsContent>
        <TabsContent value="received" forceMount className="flex flex-col gap-3 data-[state=inactive]:hidden">
          {received}
        </TabsContent>
      </Tabs>
    </Page>
  );
}

const OUTCOME: Record<string, "success" | "warning" | "destructive"> = {
  allowed: "success",
  refused: "warning",
  failed: "destructive",
};

function Count({ children }: { children: ReactNode }) {
  return <span className="rounded-full bg-muted px-1.5 text-[0.6875rem] text-muted-foreground">{children}</span>;
}

// A tab with nothing in it yet.
function Empty({ icon, children }: { icon: ReactNode; children: ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-2 rounded-xl border border-dashed px-6 py-10 text-center [&>svg]:size-5 [&>svg]:text-subtle">
      {icon}
      <p className="m-0 max-w-md text-[0.8125rem] text-muted-foreground">{children}</p>
    </div>
  );
}
