import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { fetchPendingPeer, type Action, type PeerQuestion } from "./state";

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

  useEffect(() => {
    // Keyed on the id, not on the question object: the same question arrives again whenever the
    // forwarder resends it, and restarting the clock on each of those would be pointless churn.
    // A genuinely different question resets everything, which is what it should do.
    let cancelled = false;
    answering.current = false;
    setGone(false);
    setProblem(null);
    setBusy(false);

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
      clearInterval(timer);
    };
  }, [question.id, dispatch]);

  function answer(approved: boolean) {
    if (answering.current) return;
    answering.current = true;
    setBusy(true);
    setProblem(null);

    invoke<boolean>("answer_peer", { id: question.id, approved })
      .then((reached) => {
        // `false` is not a failure: it means that question was no longer waiting by the time the
        // answer arrived, and that nothing was approved as a result. It is the same ending the
        // recheck above finds, so it is shown the same way.
        if (reached) {
          dispatch({ kind: "peerQuestion", question: null });
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
      &ldquo;<span className="mono">{question.label}</span>&rdquo;
    </>
  );

  return (
    <main className="screen">
      <h1>Approve {name}?</h1>
      <p className="lead">
        An agent asked this computer to send a file to {name}, a machine it has never sent to
        before. Nothing is sent until you answer.
      </p>

      <section>
        <h2>Compare this fingerprint</h2>
        {/* Exactly the characters the core was given: eight groups of four, uppercase, single
            spaces. `white-space: pre-wrap` keeps them, and wrapping happens only at the spaces,
            so a group is never split across lines and selecting the value copies it whole. */}
        <p className="fingerprint">{question.fingerprint}</p>
        <p className="muted note">
          Check it group by group against the fingerprint {name} reports for itself. Zyris writes
          its own when it starts, on the line that reads{" "}
          <span className="mono">peer identity ready</span>. If the two differ anywhere, refuse.
        </p>
      </section>

      <section>
        <h2>What approving does</h2>
        <p className="note">
          This computer will send files to the name {name} from now on, and it remembers the key
          behind the fingerprint above as that name. If a different key ever answers to {name}, the
          send is refused outright rather than asked about again.
        </p>
        {/* Three times this project has shipped copy claiming more than the code does, and this
            is the screen most likely to be read as a gate on incoming files. It is not one: the
            confirmer is consulted on `send_to` and nowhere else, and the receiving side admits
            any connection whose key is on the account's node list without ever asking it. */}
        <h2>What it does not do</h2>
        <p className="note">
          It does not change what can arrive here. Any machine enrolled on your Attacca account can
          already send files to this computer, whether or not you have ever approved it.
        </p>
      </section>

      {gone ? (
        <section>
          <p>This question is no longer waiting.</p>
          <p className="muted note">
            It ran out of time, or the machine that asked gave up. Nothing was approved. If an
            agent tries the send again, you will be asked again.
          </p>
          <button
            type="button"
            className="button button-quiet"
            onClick={() => dispatch({ kind: "peerQuestion", question: null })}
          >
            Close
          </button>
        </section>
      ) : (
        <section>
          {/* Both answers the same weight. Approving is not the recommended one and refusing is
              not a failure, so neither is drawn as the thing to press. */}
          <div className="answers">
            <button
              type="button"
              className="button button-quiet"
              disabled={busy}
              onClick={() => answer(true)}
            >
              Approve
            </button>
            <button
              type="button"
              className="button button-quiet"
              disabled={busy}
              onClick={() => answer(false)}
            >
              Refuse
            </button>
          </div>
          <p className="muted note">
            Refusing approves nothing and stops this send. Leave it unanswered and the send is
            refused for you.
          </p>
          {problem && <p className="problem">{problem}</p>}
        </section>
      )}
    </main>
  );
}
