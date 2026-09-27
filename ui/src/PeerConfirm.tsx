import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { CheckIcon, InfoIcon, LaptopIcon, XIcon } from "lucide-react";
import { fetchPendingPeer, type Action, type PeerQuestion } from "./state";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { Fingerprint } from "@/components/CopyButton";
import { IconTile, Problem } from "@/components/IconTile";

// How often this screen re-asks whether the question it is showing is still waiting.
//
// Polling rather than an event, because the thing being watched for has no event to give. A
// question stops waiting three ways: the person answers it, nobody answers it in time, or the
// machine that asked gives up and its call is cancelled mid-ask. The last runs inside a Rust
// destructor during cancellation, with no place to publish from and nothing awaited, so the only
// authority on "is this still live" is the slot itself.
//
// A second is short enough that a dead button is never in front of anybody for long, and the cost
// is one IPC round trip a second while a question is on the screen and none at all otherwise — a
// question is on the screen for well under a minute, a handful of times in a machine's life.
const RECHECK_MS = 1000;

// How long both answers stay dead after the question on this screen changes.
//
// **A click decided about one question must not be able to land on another.** A question can be
// replaced under somebody's hand: the one they were reading ends — answered, expired, or its
// caller gave up — and another is installed and rendered here, in the same place, with the same
// two buttons, under a name whose length the other side chose. React reuses this component for it
// (App.tsx renders it without a `key`), so the effect below and the new label and fingerprint
// land on the same commit. Without this, Approve is live again in that same instant, and a person
// mid-click on the old question finishes the click on the new one — pinning a key nobody read.
//
// 750 ms, from the two things it has to outlast. A person who has already decided to click
// completes it within about 300 ms; a person who has not yet noticed the screen changed needs
// something like 250 to 400 ms to see it and stop. Beyond that it stops buying anything and starts
// being a dead button in front of somebody who *has* read the new fingerprint and wants to answer.
//
// The shorter of the two guards, and deliberately so. `ASK_COOL_OFF` in
// crates/zyris-app/src/confirm.rs keeps the core from installing a replacement at all for two
// seconds, which is the guard that does most of the work; this one covers the click already on its
// way down when a swap does reach the screen. Nothing checks the two numbers against each other.
const SWAP_LOCKOUT_MS = 750;

// A rejected `invoke` carries whatever the command returned as its error. `answer_peer` returns a
// boolean and never an error, so anything here means the bridge itself broke.
function asMessage(error: unknown, fallback: string): string {
  return typeof error === "string" ? error : fallback;
}

// The screen a person approves a machine on.
//
// It takes the window over — App.tsx renders it instead of whatever was showing — the way
// onboarding does, and for the same reason: it is not a place anybody navigates to, it is
// somewhere the core put them, and there is exactly one thing to do there. A fifth sidebar entry
// would be wrong twice over: it would be empty almost always, and it would make a question with a
// deadline on it something a person could wander past.
//
// Whatever screen was underneath is untouched, because the question lives beside `screen` in the
// reducer rather than in it. Answering hands the person back exactly where they were.
export function PeerConfirm({
  question,
  dispatch,
}: {
  question: PeerQuestion;
  dispatch: (action: Action) => void;
}) {
  // The question stopped waiting while this screen was open. Not an error — it is the ordinary
  // end of a question nobody got to in time — so it replaces the two buttons with a sentence
  // rather than colouring anything red.
  const [gone, setGone] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  // True from the moment a button is pressed until its answer comes back. It disables both
  // buttons, so a double click cannot send a second answer, and it silences the recheck below:
  // a poll landing mid-answer sees a slot that has already been emptied by the answer itself and
  // would flash "no longer waiting" a moment before the screen closes on a successful approval.
  const answering = useRef(false);
  const [busy, setBusy] = useState(false);
  // True for SWAP_LOCKOUT_MS after the question on this screen changes, including the first time
  // it appears. Both buttons are disabled while it holds.
  //
  // Kept twice on purpose. The state is what the render reads, and the ref is what `answer` reads:
  // a handler closes over the state from the render it was created in, and the one render whose
  // value must not be trusted is the one this is guarding against. A disabled button emits no
  // click event, so the ref is a belt to the state's braces rather than the mechanism — but the
  // mechanism here is one `disabled` attribute away from a security bug, and this is the cheaper
  // of the two ways to find that out.
  const locked = useRef(true);
  const [settling, setSettling] = useState(true);
  // Which question the lockout above was armed for.
  const [lockedFor, setLockedFor] = useState(question.id);

  // **Everything about the previous question is dropped during the render, not from an effect.**
  // A passive effect runs *after* the browser has painted, so a reset done there leaves one
  // painted frame in which this screen shows the new question's name and fingerprint while still
  // carrying the last one's state — both answers live again, or the "no longer waiting" line and
  // its Close button sitting over a question that is very much waiting. That frame is exactly the
  // one a click already on its way down lands in. Adjusting state during render is what React
  // documents for a value that has to follow a prop within the same commit: it re-renders
  // immediately, before anything reaches the DOM.
  if (lockedFor !== question.id) {
    setLockedFor(question.id);
    setSettling(true);
    setGone(false);
    setProblem(null);
    setBusy(false);
    locked.current = true;
    answering.current = false;
  }

  useEffect(() => {
    // Keyed on the id, not on the question object: the same question arrives again whenever the
    // forwarder resends it, and restarting the clock on each of those would be pointless churn.
    let cancelled = false;

    // The other end of the lockout armed during the render above: this is only what lets it go.
    // Keyed on the question's id like everything else in this effect, so the beat runs once per
    // question rather than once per render.
    const lockout = setTimeout(() => {
      if (cancelled) return;
      locked.current = false;
      setSettling(false);
    }, SWAP_LOCKOUT_MS);

    const timer = setInterval(() => {
      if (answering.current) return;
      void fetchPendingPeer()
        .then((waiting) => {
          // `cancelled` covers the effect having been torn down — a new question, or the screen
          // closing — between asking and answering. Without it a stale `null` would end a
          // question that is very much alive.
          if (cancelled || answering.current) return;
          if (!waiting) {
            setGone(true);
            return;
          }
          // A different question is waiting, which means the event announcing it did not reach
          // this window. Rare enough to be nearly theoretical and cheap enough to handle: show
          // the one that is actually waiting rather than the one that is not.
          if (waiting.id !== question.id) dispatch({ kind: "peerQuestion", question: waiting });
        })
        .catch(() => {
          // Nothing to say. A failed check is not evidence the question ended, and treating it
          // as such would take a live Approve button away from somebody halfway through
          // comparing a fingerprint. The next tick asks again.
        });
    }, RECHECK_MS);

    return () => {
      cancelled = true;
      clearTimeout(lockout);
      clearInterval(timer);
    };
  }, [question.id, dispatch]);

  function answer(approved: boolean) {
    if (answering.current || locked.current) return;
    answering.current = true;
    setBusy(true);
    setProblem(null);

    invoke<boolean>("answer_peer", { id: question.id, approved })
      .then((reached) => {
        // `false` is not a failure: it means that question was no longer waiting by the time the
        // answer arrived, and that nothing was approved as a result. It is the same ending the
        // recheck above finds, so it is shown the same way.
        if (reached) {
          // **Named, not just cleared.** `Pending::answer` empties the slot before this reply
          // leaves the process, so another question can be installed and announced in the gap —
          // and its event can reach the reducer ahead of this. An unconditional clear would blank
          // that question instead, unmount this screen, stop the poll above with it, and leave a
          // live question holding the slot unseen until it expired. The reducer drops this on the
          // floor unless the question it names is still the one showing.
          dispatch({ kind: "peerQuestionEnded", id: question.id });
          return;
        }
        answering.current = false;
        setBusy(false);
        setGone(true);
      })
      .catch((error: unknown) => {
        // The answer never left this window, so the question may well still be waiting. Let them
        // press again.
        answering.current = false;
        setBusy(false);
        setProblem(asMessage(error, "Could not send your answer."));
      });
  }

  // Quoted and in a monospace face everywhere it appears. The name came from whoever called
  // `send_to`, so it is untrusted text sitting in the middle of sentences about trust: a label
  // reading "this machine is already approved" must not be able to pass for the screen's own
  // words. The quotes and the face are where that sentence ends and this one begins.
  const name = (
    <>
      &ldquo;<span className="font-mono">{question.label}</span>&rdquo;
    </>
  );

  // Open for as long as a question is waiting, and not dismissable: Escape or a click outside
  // would leave an agent's send blocked on a question nobody can see. It ends by being answered,
  // by running out of time, or — once it has gone — by Close.
  const stay = (event: Event) => event.preventDefault();

  return (
    <Dialog open>
      <DialogContent onEscapeKeyDown={stay} onPointerDownOutside={stay} onInteractOutside={stay}>
        <div className="flex items-start gap-3.5">
          <IconTile className="size-10 rounded-[0.625rem] border-primary/30 bg-primary/10 [&_svg]:size-[1.1875rem]">
            <LaptopIcon />
          </IconTile>
          <div className="flex min-w-0 flex-col gap-1.5">
            <DialogTitle>Approve {name}?</DialogTitle>
            <DialogDescription>
              An agent asked this computer to send a file to {name}, a machine it has never sent to before.
              Nothing is sent until you answer.
            </DialogDescription>
          </div>
        </div>

        <section className="flex flex-col gap-2.5" aria-label="Compare this fingerprint">
          <h3 className="m-0 text-[0.78125rem] font-normal text-muted-foreground">
            Compare this fingerprint with the one on that machine's Status screen
          </h3>
          {/* Exactly the characters the core was given: eight groups of four, uppercase, single
              spaces. Selecting it copies it whole. */}
          <Fingerprint value={question.fingerprint} size="md" />
          <p className="m-0 text-xs text-subtle">
            If any group differs, refuse. A machine running <span className="font-mono">--headless</span> writes
            the same value to its log on the line that reads <span className="font-mono">peer identity ready</span>.
          </p>
        </section>

        <div className="flex flex-col gap-2 rounded-lg border border-sidebar-border bg-inset px-3.5 py-3 text-[0.8125rem] leading-relaxed text-muted-foreground">
          <p className="m-0 flex gap-2">
            <CheckIcon className="mt-0.5 size-4 shrink-0 text-success" aria-hidden="true" />
            <span>
              Approving lets this computer send files to {name} from now on. If a different key ever answers
              to that name, the send is refused.
            </span>
          </p>
          {/* The screen most likely to be read as a gate on incoming files. It is not one. */}
          <p className="m-0 flex gap-2">
            <InfoIcon className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
            <span>
              It does not change what can arrive here: any machine on your Attacca account can already send
              files to this computer.
            </span>
          </p>
        </div>

        {gone ? (
          <div className="flex flex-col gap-3">
            <p className="m-0 text-sm text-heading">This question is no longer waiting.</p>
            <p className="m-0 text-[0.8125rem] text-muted-foreground">
              It ran out of time, or the machine that asked gave up. Nothing was approved. If an agent tries
              again, you will be asked again.
            </p>
            <Button variant="outline" onClick={() => dispatch({ kind: "peerQuestionEnded", id: question.id })}>
              Close
            </Button>
          </div>
        ) : (
          <div className="flex flex-col gap-3">
            {/* Both answers the same weight. Approving is not the recommended one and refusing is
                not a failure, so neither is drawn as the thing to press. */}
            <div className="grid grid-cols-2 gap-2.5">
              <Button variant="outline" size="lg" disabled={busy || settling} onClick={() => answer(false)}>
                <XIcon />
                Refuse
              </Button>
              <Button variant="outline" size="lg" disabled={busy || settling} onClick={() => answer(true)}>
                <CheckIcon />
                Approve
              </Button>
            </div>
            <p className="m-0 text-center text-xs text-subtle">
              Refusing approves nothing and stops this send. Leave it unanswered and it is refused for you.
            </p>
            {problem && <Problem className="text-center">{problem}</Problem>}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
