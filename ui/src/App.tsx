import { useEffect, useReducer } from "react";
import { Mcp } from "./Mcp";
import { Onboarding } from "./Onboarding";
import { PeerConfirm } from "./PeerConfirm";
import { Settings } from "./Settings";
import { Status } from "./Status";
import { Tools } from "./Tools";
import {
  fetchLatestEvent,
  fetchPendingPeer,
  initialState,
  reduce,
  subscribe,
  TABS,
  type Screen,
  type Tab,
} from "./state";

// The list of screens lives in state.ts beside the `Tab` type it defines — see the comment there.
// MCP sits beside Tools rather than inside it: both are about what this machine hands an agent,
// but one of them is a list of processes with switches on it and the other is a record of what
// ran, and the Tools screen was already three sections long.
//
// No router. There are no URLs here — the window is one process with one screen showing — so a
// router would be a dependency, a history stack and a set of paths to keep in step, in exchange
// for what an equality check already does.
function Sidebar({ screen, onNavigate }: { screen: Screen; onNavigate: (to: Tab) => void }) {
  return (
    <nav className="sidebar" aria-label="Screens">
      {TABS.map((tab) => (
        <button
          key={tab.id}
          type="button"
          // Exactly one item is current, decided by equality against the screen that is showing.
          className={tab.id === screen ? "tab tab-on" : "tab"}
          aria-current={tab.id === screen ? "page" : undefined}
          onClick={() => onNavigate(tab.id)}
        >
          {tab.label}
        </button>
      ))}
    </nav>
  );
}

export function App() {
  const [state, dispatch] = useReducer(reduce, initialState);

  useEffect(() => {
    // subscribe resolves to an unlisten function; React may run this effect twice in StrictMode,
    // so the cleanup has to cover a subscription that is still being set up.
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void subscribe(dispatch).then((fn) => {
      if (cancelled) {
        fn();
        return;
      }
      unlisten = fn;
      // The listener is now registered, but everything the core published before this instant
      // is already gone — `emit` only reaches listeners that exist, and nothing here replays a
      // send. Ask once for whatever the core last published and fold it in through the same
      // reducer; anything also delivered live through `subscribe` arrives twice, which `reduce`
      // is written to tolerate (see its comment in state.ts).
      void fetchLatestEvent().then((event) => {
        if (!cancelled && event) dispatch(event);
      });
      // And the other thing a window can arrive too late for. A peer question is published
      // transiently, so it is never in the one-slot value above — and this window may well have
      // been *raised by* that question, which means the core published it while the webview was
      // still starting. An event alone would lose it to exactly the case it exists for.
      //
      // Only a question is folded in, never the absence of one. `null` is what the command says
      // whenever nothing is waiting, which is almost always, and it is already the initial state
      // — so applying it would buy nothing and would cost a race: a question arriving live in the
      // gap between this call and its answer would be wiped out by an answer that predates it.
      // `Action` now says so as well: `peerQuestion` cannot carry an absence, and the one action
      // that clears a question has to name which one it is clearing.
      void fetchPendingPeer().then((question) => {
        if (!cancelled && question) dispatch({ kind: "peerQuestion", question });
      });
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Over everything, including onboarding, and it does not touch `screen`: the screen underneath
  // is handed back untouched the moment the question is answered. First because it is the only
  // thing here with a deadline — an agent's send is blocked on it and refuses itself if nobody
  // answers — and because it cannot collide with onboarding in practice anyway: a question comes
  // from an agent, an agent needs a connection, and a connection needs this machine enrolled.
  if (state.question) return <PeerConfirm question={state.question} dispatch={dispatch} />;
  if (state.screen === "onboarding") return <Onboarding state={state} />;
  // The sidebar appears only once this machine is enrolled: before that there is nothing to
  // navigate to, and offering a choice of screens to someone who has not authorized the computer
  // yet is offering them a way to miss the one thing they have to do.
  //
  // Named by what it is *not* — onboarding having already returned above, `starting` is the only
  // screen left that is not a tab — so that adding a `Tab` cannot leave a screen the sidebar
  // offers and this branch drops through, which would be the starting screen with no way back to
  // anywhere. `pastEnrolment` in state.ts names the two it claims for the same reason.
  if (state.screen !== "starting") {
    return (
      <div className="shell">
        <Sidebar screen={state.screen} onNavigate={(to) => dispatch({ kind: "navigate", to })} />
        {state.screen === "status" && <Status state={state} />}
        {state.screen === "tools" && <Tools state={state} dispatch={dispatch} />}
        {/* Only `state` in: what this screen lists is read through a command, and the one thing
            it needs from the core is the news that a server changed — a death in particular, which
            nobody clicked and which nothing else would bring to the screen. */}
        {state.screen === "mcp" && <Mcp state={state} />}
        {/* No props: what this screen shows is read off the machine through a command, not
            folded into core state, because nothing outside it needs the answer. */}
        {state.screen === "settings" && <Settings />}
      </div>
    );
  }
  return (
    <main className="screen">
      <p className="muted">Starting.</p>
    </main>
  );
}
