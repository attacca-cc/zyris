import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { State } from "./state";

// This computer's own peer fingerprint, as the `peer_fingerprint` command answers with it — the
// same string `Peering::fingerprint` hands the approval screen about somebody else's machine, and
// the same one that machine's own Status screen shows about this one.
//
// Three answers, like the inbox read on the Tools screen and for the same reason. `undefined` is a
// read still in flight, `null` is a computer with no peer identity — no key, no `file_transfer`,
// nothing to compare — and a string is the value. An empty string is none of those and must never
// be shown as a fingerprint made of nothing.
type Fingerprint = string | null | undefined;

export function Status({ state }: { state: State }) {
  const [fingerprint, setFingerprint] = useState<Fingerprint>(undefined);
  const [fingerprintProblem, setFingerprintProblem] = useState<string | null>(null);

  // Mount only, and that is the whole of it: this is computed once when the endpoint binds and
  // cannot change while the process runs — the same key is the same fingerprint, which is the
  // property the key is persisted for. Nothing to re-read on focus the way Settings has to.
  useEffect(() => {
    // The same guard the other screens use: StrictMode runs this effect twice and the answer can
    // land after the cleanup.
    let cancelled = false;

    void invoke<string | null>("peer_fingerprint")
      .then((answer) => {
        if (!cancelled) setFingerprint(answer);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        // Never `setFingerprint(null)` on a failed read. "This computer has no peer identity" is a
        // sentence about the machine; a read that did not come back says nothing at all, and the
        // one thing this screen must not do is answer a question it could not read.
        setFingerprintProblem(
          typeof error === "string" ? error : "Could not read this computer's fingerprint.",
        );
      });

    return () => {
      cancelled = true;
    };
  }, []);

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

      {/* **The other half of the approval screen, and it lives here rather than beside the node
          id above.** Somebody reaches this section because a different computer is asking them to
          approve this one and they have been told to come and read the value off it. That works
          before this machine has ever connected — the key is on disk and the endpoint binds
          without a network — so it must not be inside the `state.node` branch, which is empty
          until Attacca has answered.

          Not in the `facts` list either, for the opposite reason: the two values above come from
          Attacca and name this node on the account, and this one comes from a key file on this
          disk and names this machine to its peers. Putting a third `dt` under the same `dl` would
          invite exactly the mix-up a person is least able to recover from — reading the node id
          aloud, character by character, against a fingerprint. */}
      <section>
        <h2>This computer&rsquo;s fingerprint</h2>
        {fingerprintProblem ? (
          <p className="problem">{fingerprintProblem}</p>
        ) : fingerprint === undefined ? (
          <p className="muted">Reading this computer&rsquo;s fingerprint.</p>
        ) : fingerprint === null ? (
          <p className="muted">
            File transfer is not running on this computer, so it has no fingerprint and no other
            machine can send a file here. Zyris says why in its log when it starts.
          </p>
        ) : (
          <>
            {/* The same face and spacing the approval screen uses, because the two are read
                against each other group by group. `white-space: pre-wrap` keeps the single
                spaces, wrapping happens only at them, and selecting the value copies it whole. */}
            <p className="fingerprint">{fingerprint}</p>
            <p className="muted note">
              Read this out when another of your machines asks you to approve this one. It is the
              same after every restart, and it is not a secret — it is the short form of this
              computer&rsquo;s public key, and the only thing it is good for is telling this
              machine apart from one pretending to be it.
            </p>
          </>
        )}
      </section>
    </main>
  );
}
