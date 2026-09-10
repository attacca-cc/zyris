import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { State } from "./state";

export function Onboarding({ state }: { state: State }) {
  const code = state.code;

  // Local, UI-only feedback for a failed "open the browser" click — the core has no notion
  // of this, so it does not belong in `state`. It shares the same problem slot as
  // `state.problem` so the screen has one spot for things that went wrong, not two.
  const [openError, setOpenError] = useState<string | null>(null);

  // Guards a rejection that arrives after what it was answering is no longer current.
  // `generation` is bumped whenever the reducer hands down a new `state` object — which is
  // every real transition (the reducer only ever returns the *same* reference for events
  // this screen doesn't care about) — not only when `code` itself changes. An in-flight open
  // attempt captures the generation it started with and, when it settles, reports its
  // outcome only if nothing has moved on since; otherwise it stays quiet rather than
  // flashing an answer to a question nobody is asking any more. `mounted` guards the same
  // way against the screen having unmounted entirely (e.g. the core reached "connected").
  const guard = useRef({ generation: 0, mounted: true });

  useEffect(() => {
    guard.current.generation += 1;
    setOpenError(null);
  }, [state]);

  useEffect(() => {
    return () => {
      guard.current.mounted = false;
    };
  }, []);

  function openVerificationUrl(url: string) {
    setOpenError(null);
    // A fresh click also supersedes whatever attempt came before it, same as a core
    // transition does — the person asking again is a new question, not a continuation.
    guard.current.generation += 1;
    const attempt = guard.current.generation;
    invoke("open_verification_url", { url }).catch((error) => {
      if (!guard.current.mounted || guard.current.generation !== attempt) return;
      setOpenError(typeof error === "string" ? error : "Could not open the browser.");
    });
  }

  const problem = state.problem ?? openError;

  return (
    <main className="screen">
      <h1>Authorize this computer</h1>
      <p className="lead">
        Zyris hands this machine to your Attacca agents. Approve it once and it reconnects on its
        own from then on.
      </p>

      {code ? (
        <>
          <ol className="steps">
            <li>
              Open{" "}
              <button className="link" onClick={() => openVerificationUrl(code.verificationUri)}>
                {code.verificationUri}
              </button>
            </li>
            <li>
              Enter this code: <code className="code">{code.userCode}</code>
            </li>
          </ol>
          <p className="muted">Waiting for approval.</p>
        </>
      ) : (
        // Do not say "asking" and "failed" at once: once a problem is known, this placeholder
        // steps aside and lets the problem message below speak for the screen.
        !state.problem && <p className="muted">Asking Attacca for a code.</p>
      )}

      {problem && <p className="problem">{problem}</p>}
    </main>
  );
}
