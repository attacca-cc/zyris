import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

// What the `autostart_state` and `set_autostart` commands answer with: `AutostartView` in
// crates/zyris-app/src/bridge.rs, serialized camelCase, with `zyris_autostart::State` inside
// it. That enum is externally tagged, so two of its three states are bare strings and the
// third is an object carrying the reason. A test in bridge.rs pins that exact JSON; this is the
// other half of the agreement, and nothing checks the two at build time.
type AutostartState = "enabled" | "disabled" | { unsupported: string };

type Autostart = {
  state: AutostartState;
  caveats: string[];
  mechanism: string | null;
};

// A rejected `invoke` carries whatever the command returned as its error. These commands return
// strings, so anything else means the bridge itself broke and is not worth showing verbatim.
// The same three lines as in Tools.tsx, for the same reason.
function asMessage(error: unknown, fallback: string): string {
  return typeof error === "string" ? error : fallback;
}

function unsupportedReason(state: AutostartState): string | null {
  return typeof state === "object" ? state.unsupported : null;
}

export function Settings() {
  const [autostart, setAutostart] = useState<Autostart | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  // A `schtasks` call or three `systemctl` calls are not instant. Without this the button can
  // be pressed again while the first press is still installing.
  const [busy, setBusy] = useState(false);

  // Read on every visit to this screen *and* every time the window comes back to the front.
  // Somebody can delete the unit or the task by hand, and the switch has to follow the machine
  // rather than the last thing Zyris did.
  //
  // Two triggers because neither covers the other. Mounting covers navigation: `App.tsx`
  // renders this screen conditionally, so leaving Settings unmounts it and coming back runs
  // this effect again. Focus covers what that misses — closing the window only hides it, so
  // somebody whose last screen was this one reopens it days later on the same stale answer,
  // and autostart is exactly what makes those days long. The read is one `spawn_blocking`
  // round trip that never touches the UI thread, so doing it again is close to free.
  useEffect(() => {
    // The same guard the other screens use — StrictMode runs this effect twice, and both the
    // read and the subscription can land after the cleanup.
    let cancelled = false;

    function read() {
      void invoke<Autostart>("autostart_state")
        .then((answer) => {
          if (cancelled) return;
          setAutostart(answer);
          // An answer is an answer: a message from a read that failed earlier has stopped
          // being true, and leaving it under a working switch reads as a broken one.
          setProblem(null);
        })
        .catch((error: unknown) => {
          if (!cancelled) {
            setProblem(
              asMessage(error, "Could not read whether Zyris starts with this computer."),
            );
          }
        });
    }

    read();

    let unlisten: UnlistenFn | undefined;
    void getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        // Only on the way in. This fires on blur too, and reading as somebody clicks away
        // would spend a `systemctl` round trip on every alt-tab for an answer nobody sees.
        if (focused) read();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  function toggle(enabled: boolean) {
    setProblem(null);
    setBusy(true);
    void invoke<Autostart>("set_autostart", { enabled })
      .then(setAutostart)
      .catch((error: unknown) => {
        setProblem(
          asMessage(
            error,
            enabled ? "Could not turn this on." : "Could not turn this off.",
          ),
        );
        // Ask the machine again rather than leaving the switch where it was. Turning autostart
        // on can fail after it has already written something, and a switch showing what was
        // asked for instead of what happened is the one thing this screen must not do.
        return invoke<Autostart>("autostart_state")
          .then(setAutostart)
          .catch(() => {
            // Nothing to add: the message above is already the honest one, and a second
            // failure says the same thing twice.
          });
      })
      .finally(() => setBusy(false));
  }

  const reason = autostart ? unsupportedReason(autostart.state) : null;
  const on = autostart?.state === "enabled";

  return (
    <main className="screen">
      <h1>Settings</h1>
      <p className="lead">What this computer does when nobody has Zyris open.</p>

      <section>
        <h2>Start with this computer</h2>

        {autostart === null ? (
          <p className={problem ? "problem" : "muted"}>
            {problem ?? "Reading whether Zyris starts with this computer."}
          </p>
        ) : reason !== null ? (
          // Never a switch that will not move with nothing beside it. The reason is the whole
          // of what a person can act on here.
          <div className="panel">
            <p className="switch-state">
              <span className="dot dot-off" aria-hidden="true" />
              Zyris cannot start itself on this computer.
            </p>
            <p className="muted note">{reason}</p>
          </div>
        ) : (
          <div className="panel">
            <div className="switch-row">
              <p className="switch-state">
                <span className={on ? "dot dot-on" : "dot dot-off"} aria-hidden="true" />
                {on
                  ? "On. Zyris starts when you sign in, and reconnects on its own."
                  : "Off. Zyris runs only when you start it yourself."}
              </p>
              <button type="button" className="button" onClick={() => toggle(!on)} disabled={busy}>
                {on ? "Turn off" : "Turn on"}
              </button>
            </div>

            {/* Named, because somebody who wants to undo this without Zyris in front of them
                has to know what to go and look for. */}
            {autostart.mechanism && (
              <p className="muted note">
                {on
                  ? `It starts through ${autostart.mechanism}.`
                  : `Turning this on adds ${autostart.mechanism}.`}
              </p>
            )}

            {/* Everything true of this machine that leaves the switch weaker than "on" sounds.
                On Linux this is where a person learns that the unit needs a graphical session,
                so a computer switched on with nobody logged in is not connected. Which
                sentences these are is the backend's to say — this screen cannot tell what
                platform it is on, and guessing from the mechanism string would be a second
                place for the answer to live. */}
            {autostart.caveats.map((caveat) => (
              <p className="warn note" key={caveat}>
                {caveat}
              </p>
            ))}

            {problem && <p className="problem">{problem}</p>}
          </div>
        )}

        {autostart !== null && reason === null && (
          // Two things a person would otherwise find out by being surprised: started this way
          // there is no window on the screen, and this switch is not the one that stops agents.
          //
          // "Where did it go" is the question this answers, and it is the whole reason Zyris
          // starts itself with a hidden window rather than headless: a headless Zyris has no
          // tray icon, and launching it again reaches a process with nothing listening.
          <p className="muted note">
            Started this way Zyris opens no window. Its tray icon brings one up, and so does
            starting Zyris again. It does not change what agents can reach on this computer —
            that is the switch on the Tools screen.
          </p>
        )}
      </section>
    </main>
  );
}
