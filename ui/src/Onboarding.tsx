import { invoke } from "@tauri-apps/api/core";
import type { State } from "./state";

export function Onboarding({ state }: { state: State }) {
  const code = state.code;

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
              <button
                className="link"
                onClick={() => void invoke("open_verification_url", { url: code.verificationUri })}
              >
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
        <p className="muted">Asking Attacca for a code.</p>
      )}

      {state.problem && <p className="problem">{state.problem}</p>}
    </main>
  );
}
