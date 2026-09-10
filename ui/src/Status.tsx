import type { State } from "./state";

export function Status({ state }: { state: State }) {
  return (
    <main className="screen">
      <header className="status-head">
        {/* Freshness is an indicator's job; the text says what is true, not how long ago. */}
        <span className={state.connected ? "dot dot-on" : "dot dot-off"} aria-hidden="true" />
        <h1>{state.connected ? "Connected" : "Not connected"}</h1>
      </header>

      {state.node ? (
        <dl className="facts">
          <dt>Node</dt>
          <dd>{state.node.nodeName}</dd>
          <dt>Id</dt>
          <dd className="mono">{state.node.nodeId}</dd>
        </dl>
      ) : (
        <p className="muted">This node has not connected yet.</p>
      )}

      {!state.connected && state.problem && (
        <p className="problem">
          {state.problem}
          <br />
          <span className="muted">
            {state.retrying
              ? "Zyris keeps trying on its own."
              : "Zyris has stopped trying. Restart it to reconnect."}
          </span>
        </p>
      )}
    </main>
  );
}
