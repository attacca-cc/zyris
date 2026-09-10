import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { State } from "./state";

export function Onboarding({ state }: { state: State }) {
  const code = state.code;

  // Local, UI-only feedback for a failed "open the browser" click — the core has no notion
  // of this, so it does not belong in `state`. It shares the same problem slot as
  // `state.problem` so the screen has one spot for things that went wrong, not two.
  const [openError, setOpenError] = useState<string | null>(null);

  // A new (or cleared) code makes any earlier "could not open the browser" message stale.
  useEffect(() => {
    setOpenError(null);
  }, [code?.userCode]);

  function openVerificationUrl(url: string) {
    setOpenError(null);
    invoke("open_verification_url", { url }).catch((error) => {
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
