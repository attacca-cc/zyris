import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { BotIcon, ChevronRightIcon, FolderIcon, PlusIcon, RefreshCwIcon } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { cn } from "@/lib/utils";

// Which Attacca session this machine talks to: a project, then a session in it, or a new one.
//
// **One session at a time, for the whole machine.** What is heard is sent to it and what it
// answers is read aloud, and the choice is written into `voice.json` so the next launch comes back
// to it. Switching does not bring the old conversation along: the turn feed is live-only, so the
// screen starts empty on the new session rather than showing turns that belong to another.

// `zyris_voice::view::SessionsView`.
export type SessionsView = {
  projects: { id: string; name: string; isDefault: boolean }[];
  sessions: {
    id: string;
    title: string | null;
    project: string | null;
    agent: string | null;
    running: boolean;
  }[];
  agents: { id: string; name: string }[];
  current: string | null;
  // One sentence per list that could not be read, usually a scope the credential lacks.
  problems: string[];
};

type Session = SessionsView["sessions"][number];

// A session with no project was filed under the account's default one.
function projectOf(session: Session, view: SessionsView): string | null {
  return session.project ?? view.projects.find((p) => p.isDefault)?.id ?? null;
}

// Attacca names a session from its first message, so one that has not had one yet has no title.
function titleOf(session: Session): string {
  return session.title && session.title.trim() !== ""
    ? session.title
    : `Untitled session (${session.id.slice(0, 8)})`;
}

// Who answers in the current session: its own agent, or the one a new session would get.
function agentNameOf(view: SessionsView, chosen: string | null): string | null {
  const current = view.sessions.find((s) => s.id === view.current);
  const id = current?.agent ?? chosen;
  return view.agents.find((a) => a.id === id)?.name ?? null;
}

function asMessage(error: unknown, fallback: string): string {
  return typeof error === "string" ? error : fallback;
}

export function SessionPicker({
  hidden,
  onSwitched,
  onAgentName,
}: {
  hidden: boolean;
  // Called once the machine is talking to a different session than before.
  onSwitched: () => void;
  // The name of the agent the current session talks to, for the thread to put over its answers.
  onAgentName?: (name: string | null) => void;
}) {
  const [view, setView] = useState<SessionsView | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [project, setProject] = useState<string | null>(null);
  const [agent, setAgent] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Take an answer from the Rust side, and keep the project dropdown on something that exists.
  const take = useCallback((next: SessionsView) => {
    setView(next);
    setProblem(null);
    setProject((chosen) => {
      if (chosen !== null && next.projects.some((p) => p.id === chosen)) return chosen;
      const current = next.sessions.find((s) => s.id === next.current);
      return (
        (current && projectOf(current, next)) ??
        next.projects.find((p) => p.isDefault)?.id ??
        next.projects[0]?.id ??
        null
      );
    });
    setAgent((chosen) => {
      if (chosen !== null && next.agents.some((a) => a.id === chosen)) return chosen;
      const current = next.sessions.find((s) => s.id === next.current);
      return current?.agent ?? next.agents[0]?.id ?? null;
    });
  }, []);

  const load = useCallback(() => {
    invoke<SessionsView>("conversation_sessions")
      .then(take)
      .catch((error) => setProblem(asMessage(error, "The sessions could not be read.")));
  }, [take]);

  // Read again whenever the tab is shown: sessions are made and renamed in the web app too.
  useEffect(() => {
    if (!hidden) load();
  }, [hidden, load]);

  // Not connected yet is the ordinary first answer, so keep asking while it is the answer.
  useEffect(() => {
    if (hidden || problem === null) return;
    const again = setInterval(load, 3000);
    return () => clearInterval(again);
  }, [hidden, problem, load]);

  const act = (command: string, args: Record<string, unknown>) => {
    const before = view?.current ?? null;
    setBusy(true);
    invoke<SessionsView>(command, args)
      .then((next) => {
        take(next);
        if (next.current !== before) onSwitched();
      })
      .catch((error) => setProblem(asMessage(error, "That did not work.")))
      .finally(() => setBusy(false));
  };

  // Above the early return, as every hook must be.
  const agentName = view ? agentNameOf(view, agent) : null;
  useEffect(() => {
    onAgentName?.(agentName);
  }, [agentName, onAgentName]);

  if (view === null) {
    return (
      <div className="flex min-h-14 items-center border-b border-[#1d1814] px-6">
        <p className={cn("m-0 text-[0.8125rem]", problem ? "text-[#ef7d75]" : "text-muted-foreground")}>
          {problem ?? "Reading this account's sessions…"}
        </p>
      </div>
    );
  }

  // A credential without `projects:read` still has sessions to choose from: with no projects to
  // group them by, every session is listed and a new one goes to the account's default project.
  const byProject = view.projects.length > 0;
  const inProject = byProject
    ? view.sessions.filter((s) => projectOf(s, view) === project)
    : view.sessions;
  const current = view.sessions.find((s) => s.id === view.current);
  // The current session stays selectable even when it is in another project or not listed, so
  // the dropdown never shows some other session as if it were the one in use.
  const showCurrentApart = view.current !== null && !inProject.some((s) => s.id === view.current);
  const notes = [
    ...(view.agents.length === 0 ? ["This account has no agent, so no session can be started."] : []),
    ...(inProject.length === 0 && !showCurrentApart
      ? [byProject ? "There are no sessions in this project yet." : "There are no sessions yet."]
      : []),
    ...view.problems.map((line) => `Could not read ${line}`),
    ...(!byProject && view.problems.some((line) => line.startsWith("projects"))
      ? ["Sessions from every project are listed, and a new session goes to the default project."]
      : []),
  ];

  return (
    <div className="border-b border-[#1d1814] bg-background">
      <div className="flex min-h-14 flex-wrap items-center gap-2 px-6 py-2.5">
        {byProject && (
          <>
            <Select value={project ?? undefined} disabled={busy} onValueChange={setProject}>
              <SelectTrigger size="sm" aria-label="Project" className="w-auto max-w-44 bg-transparent">
                <FolderIcon className="text-muted-foreground" aria-hidden="true" />
                <SelectValue placeholder="Project" />
              </SelectTrigger>
              <SelectContent>
                {view.projects.map((p) => (
                  <SelectItem key={p.id} value={p.id}>
                    {p.name}
                    {p.isDefault ? " (default)" : ""}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <ChevronRightIcon className="size-3.5 text-[#4a3f37]" aria-hidden="true" />
          </>
        )}
        <Select
          value={view.current ?? undefined}
          disabled={busy}
          onValueChange={(session) => act("choose_conversation_session", { session })}
        >
          <SelectTrigger size="sm" aria-label="Session" className="w-auto max-w-80 min-w-44 bg-transparent font-medium">
            <SelectValue placeholder="No session yet" />
          </SelectTrigger>
          <SelectContent>
            {showCurrentApart && view.current !== null && (
              <SelectItem value={view.current}>
                {current ? titleOf(current) : view.current} (another project)
              </SelectItem>
            )}
            {inProject.map((s) => (
              <SelectItem key={s.id} value={s.id}>
                {titleOf(s)}
                {s.running ? " — answering" : ""}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {agentName && view.agents.length === 1 && (
          <Badge variant="secondary" title="The agent new sessions talk to">
            {agentName}
          </Badge>
        )}

        <div className="flex-1" />

        {view.agents.length > 1 && (
          <Select value={agent ?? undefined} disabled={busy} onValueChange={setAgent}>
            <SelectTrigger size="sm" aria-label="Agent for a new session" className="w-auto max-w-40 bg-transparent">
              <BotIcon className="text-muted-foreground" aria-hidden="true" />
              <SelectValue placeholder="Agent" />
            </SelectTrigger>
            <SelectContent>
              {view.agents.map((a) => (
                <SelectItem key={a.id} value={a.id}>
                  {a.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Refresh"
          title="Read the sessions again"
          disabled={busy}
          onClick={load}
        >
          <RefreshCwIcon />
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={busy || (byProject && project === null) || view.agents.length === 0}
          onClick={() =>
            act("new_conversation_session", {
              project: byProject ? project : null,
              agent: view.agents.length > 1 ? agent : null,
            })
          }
        >
          <PlusIcon />
          New session
        </Button>
      </div>
      {(notes.length > 0 || problem) && (
        <div className="flex flex-col gap-0.5 px-6 pb-2.5">
          {notes.map((line) => (
            <p key={line} className="m-0 text-xs text-muted-foreground">
              {line}
            </p>
          ))}
          {problem && <p className="m-0 text-xs text-[#ef7d75]">{problem}</p>}
        </div>
      )}
    </div>
  );
}
