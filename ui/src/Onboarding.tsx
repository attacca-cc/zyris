import { useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ExternalLinkIcon, LoaderCircleIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { CopyButton } from "@/components/CopyButton";
import { Note, Problem } from "@/components/IconTile";
import { Mark } from "@/components/Wordmark";
import type { State } from "./state";

export function Onboarding({ state }: { state: State }) {
  const code = state.code;

  // Local, UI-only feedback for a failed "open the browser" click — the core has no notion of
  // this, so it does not belong in `state`. Rendered in the same spot as `state.problem`, but
  // never at the same time as it and without the "restart needed" note: a core problem here
  // means the connector has already stopped, but a failed browser-open is just that, and
  // clicking again is a fine way to retry it.
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
    // StrictMode replays this effect as setup -> cleanup -> setup in development; the second
    // setup has to restore `mounted`, or it latches false for the component's whole life.
    guard.current.mounted = true;
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

  return (
    <main className="flex h-full items-center justify-center overflow-y-auto bg-[radial-gradient(900px_600px_at_50%_-10%,rgba(201,115,77,0.10),transparent_60%)] px-6 py-10">
      <div className="flex w-full max-w-[28.75rem] flex-col items-center gap-7">
        <div className="flex flex-col items-center gap-3.5 text-center">
          <Mark className="h-10" />
          <h1 className="m-0 font-display text-[1.75rem] font-semibold tracking-tight text-heading">
            Authorize this computer
          </h1>
          <p className="m-0 text-[0.90625rem] leading-relaxed text-muted-foreground">
            Zyris hands this machine to your Attacca agents. Approve it once and it reconnects on its own from
            then on.
          </p>
        </div>

        <Card className="w-full gap-5 p-6">
          {code ? (
            <>
              <Step number={1} title="Open the approval page">
                <Button size="lg" onClick={() => openVerificationUrl(code.verificationUri)}>
                  Open {code.verificationUri.replace(/^https?:\/\//, "")}
                  <ExternalLinkIcon />
                </Button>
              </Step>
              <Step number={2} title="Enter this code">
                <div className="flex items-center gap-2.5">
                  <code className="flex-1 rounded-[0.625rem] border bg-inset py-2.5 text-center font-mono text-[1.75rem] font-medium tracking-[0.18em] text-heading">
                    {code.userCode}
                  </code>
                  <CopyButton text={code.userCode} label="Copy the code" />
                </div>
              </Step>
              <div className="flex items-center justify-center gap-2 border-t border-sidebar-border pt-4 text-[0.8125rem] text-muted-foreground">
                <LoaderCircleIcon className="size-3.5 animate-spin text-primary" aria-hidden="true" />
                Waiting for approval
              </div>
            </>
          ) : (
            // Do not say "asking" and "failed" at once: once a problem is known, this placeholder
            // steps aside and lets the problem message below speak for the screen.
            !state.problem && (
              <div className="flex items-center justify-center gap-2 text-[0.8125rem] text-muted-foreground">
                <LoaderCircleIcon className="size-3.5 animate-spin text-primary" aria-hidden="true" />
                Asking Attacca for a code
              </div>
            )
          )}

          {state.problem ? (
            // A core-originated problem here means the connector has already stopped — unlike
            // `openError` below, clicking again will not help.
            <div className="flex flex-col gap-1 text-center">
              <Problem>{state.problem}</Problem>
              <Note>Restart Zyris to try again.</Note>
            </div>
          ) : (
            openError && <Problem className="text-center">{openError}</Problem>
          )}
        </Card>
      </div>
    </main>
  );
}

function Step({ number, title, children }: { number: number; title: string; children: ReactNode }) {
  return (
    <div className="flex gap-3.5">
      <span className="inline-flex size-6 shrink-0 items-center justify-center rounded-full border border-input bg-secondary text-xs font-semibold text-heading">
        {number}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-2.5">
        <span className="text-sm text-foreground">{title}</span>
        {children}
      </div>
    </div>
  );
}
