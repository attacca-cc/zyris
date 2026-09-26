import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

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

function asMessage(error: unknown, fallback: string): string {
  return typeof error === "string" ? error : fallback;
}

export function SessionPicker({
  hidden,
  onSwitched,
}: {
  hidden: boolean;
  // Called once the machine is talking to a different session than before.
  onSwitched: () => void;
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

  if (view === null) {
    return (
      <section className="session-picker">
        <p className="note muted">{problem ?? "Reading this account's sessions…"}</p>
      </section>
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

  return (
    <section className="session-picker">
      {byProject && (
        <label className="note">
          Project{" "}
          <select
            className="picker"
            aria-label="Project"
            value={project ?? ""}
            disabled={busy}
            onChange={(event) => setProject(event.target.value)}
          >
            {view.projects.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
                {p.isDefault ? " (default)" : ""}
              </option>
            ))}
          </select>
        </label>
      )}

      <label className="note">
        Session{" "}
        <select
          className="picker"
          aria-label="Session"
          value={view.current ?? ""}
          disabled={busy}
          onChange={(event) =>
            act("choose_conversation_session", { session: event.target.value })
          }
        >
          {view.current === null && <option value="">No session yet</option>}
          {showCurrentApart && (
            <option value={view.current ?? ""}>
              {current ? titleOf(current) : view.current} (another project)
            </option>
          )}
          {inProject.map((s) => (
            <option key={s.id} value={s.id}>
              {titleOf(s)}
              {s.running ? " — answering" : ""}
            </option>
          ))}
        </select>
      </label>

      {view.agents.length === 1 && (
        <p className="note muted">New sessions talk to {view.agents[0].name}.</p>
      )}
      {view.agents.length > 1 && (
        <label className="note">
          Agent for a new session{" "}
          <select
            className="picker"
            aria-label="Agent for a new session"
            value={agent ?? ""}
            disabled={busy}
            onChange={(event) => setAgent(event.target.value)}
          >
            {view.agents.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name}
              </option>
            ))}
          </select>
        </label>
      )}

      <div className="session-actions">
        <button
          type="button"
          className="button"
          disabled={busy || (byProject && project === null) || view.agents.length === 0}
          onClick={() =>
            act("new_conversation_session", {
              project: byProject ? project : null,
              agent: view.agents.length > 1 ? agent : null,
            })
          }
        >
          New session
        </button>
        <button type="button" className="button-quiet" disabled={busy} onClick={load}>
          Refresh
        </button>
      </div>

      {view.agents.length === 0 && (
        <p className="note muted">This account has no agent, so no session can be started.</p>
      )}
      {inProject.length === 0 && (
        <p className="note muted">
          {byProject ? "There are no sessions in this project yet." : "There are no sessions yet."}
        </p>
      )}
      {view.problems.map((line) => (
        <p key={line} className="note muted">
          Could not read {line}
        </p>
      ))}
      {!byProject && view.problems.some((line) => line.startsWith("projects")) && (
        <p className="note muted">
          Sessions from every project are listed, and a new session goes to the default project.
        </p>
      )}
      {problem && <p className="note problem">{problem}</p>}
    </section>
  );
}
