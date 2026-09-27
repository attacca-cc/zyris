import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getVersion } from "@tauri-apps/api/app";
import { InfoIcon, PowerIcon, TriangleAlertIcon } from "lucide-react";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { IconTile, Note, Problem } from "@/components/IconTile";
import { Page, PageHeader } from "@/components/PageHeader";

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

  // The version this build says it is, from `tauri.conf.json` by way of the app itself.
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    getVersion()
      .then((answer) => {
        if (!cancelled && typeof answer === "string") setVersion(answer);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const reason = autostart ? unsupportedReason(autostart.state) : null;
  const on = autostart?.state === "enabled";

  return (
    <Page>
      <PageHeader title="Settings" description="What Zyris does when nobody has it open." />

      <Card className="gap-0 overflow-hidden p-0">
        <CardHeader className="items-center px-5 py-5">
          <IconTile>
            <PowerIcon />
          </IconTile>
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <CardTitle>Start with this computer</CardTitle>
            <CardDescription>
              {autostart === null
                ? "Reading whether Zyris starts with this computer."
                : reason !== null
                  ? "Zyris cannot start itself on this computer."
                  : on
                    ? "On. Zyris starts when you sign in, and reconnects on its own."
                    : "Off. Zyris runs only when you start it yourself."}
            </CardDescription>
          </div>
          {autostart !== null && reason === null && (
            <Switch aria-label="Start with this computer" checked={on} disabled={busy} onCheckedChange={toggle} />
          )}
        </CardHeader>

        {autostart === null && problem && (
          <div className="px-5 pb-4">
            <Problem>{problem}</Problem>
          </div>
        )}
        {/* Never a switch that will not move with nothing beside it: the reason is the whole of
            what a person can act on here. */}
        {reason !== null && (
          <div className="px-5 pb-4">
            <Note>{reason}</Note>
          </div>
        )}

        {autostart !== null && reason === null && (
          <>
            {/* Everything true of this machine that leaves the switch weaker than "on" sounds. Which
                sentences these are is the backend's to say. */}
            {autostart.caveats.map((caveat) => (
              <div
                key={caveat}
                className="flex items-start gap-2.5 border-t border-warning/20 bg-warning/5 px-5 py-3 text-[0.8125rem] text-[#d9b36a]"
              >
                <TriangleAlertIcon className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
                {caveat}
              </div>
            ))}
            {problem && (
              <div className="border-t border-sidebar-border px-5 py-3">
                <Problem>{problem}</Problem>
              </div>
            )}
            <div className="flex flex-col gap-1 border-t border-sidebar-border px-5 py-3 text-xs text-subtle">
              {/* Named, because somebody undoing this without Zyris in front of them has to know
                  what to look for. */}
              {autostart.mechanism && (
                <span>{on ? `It starts through ${autostart.mechanism}.` : `Turning this on adds ${autostart.mechanism}.`}</span>
              )}
              {/* "Where did it go", and not the switch that stops agents. */}
              <span>
                Started this way Zyris opens no window — its tray icon brings one up. What agents can reach is
                the switch on the Tools screen.
              </span>
            </div>
          </>
        )}
      </Card>

      <Card>
        <CardHeader className="items-center">
          <IconTile>
            <InfoIcon />
          </IconTile>
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <CardTitle>About Zyris</CardTitle>
            <CardDescription>{version ? `Version ${version}` : "Zyris"}</CardDescription>
          </div>
        </CardHeader>
      </Card>
    </Page>
  );
}
