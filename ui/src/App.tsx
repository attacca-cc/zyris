import { useEffect, useReducer } from "react";
import { Mcp } from "./Mcp";
import { Onboarding } from "./Onboarding";
import { PeerConfirm } from "./PeerConfirm";
import { Settings } from "./Settings";
import { Status } from "./Status";
import { Tools } from "./Tools";
import { Voice } from "./Voice";
import { Conversation } from "./Conversation";
import { UpdateNotice } from "./UpdateNotice";
import { Sidebar } from "./components/Sidebar";
import {
  fetchLatestEvent,
  fetchPendingPeer,
  initialState,
  reduce,
  subscribe,
  subscribeResync,
} from "./state";

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
    // The other channel, and the reason it is a channel at all: when the forwarder finds it has
    // fallen behind on the core's events it can name the catch-up value, the switch and a waiting
    // peer question, because each of those is held somewhere it can read — but not the MCP server
    // changes it dropped, which are published transiently and kept nowhere. This says only that
    // the window's idea of things is no longer worth anything, and the screens that read through
    // a command ask again.
    let unlistenResync: (() => void) | undefined;
    void subscribeResync(() => dispatch({ kind: "resync" })).then((fn) => {
      if (cancelled) fn();
      else unlistenResync = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
      unlistenResync?.();
    };
  }, []);

  // Over everything, including onboarding, and it does not touch `screen`: the screen underneath
  // is handed back untouched the moment the question is answered. A dialog rather than a screen
  // of its own, so the person can see what they were doing when an agent asked — and it cannot
  // be dismissed except by answering, because an agent's send is blocked on it and refuses
  // itself if nobody answers.
  const overlays = (
    <>
      {state.question && <PeerConfirm question={state.question} dispatch={dispatch} />}
      <UpdateNotice />
    </>
  );

  if (state.screen === "onboarding") {
    return (
      <>
        <Onboarding state={state} />
        {overlays}
      </>
    );
  }
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
      <div className="flex h-full overflow-hidden max-sm:flex-col-reverse">
        <Sidebar
          screen={state.screen}
          state={state}
          onNavigate={(to) => dispatch({ kind: "navigate", to })}
        />
        {state.screen === "status" && <Status state={state} />}
        {state.screen === "tools" && <Tools state={state} dispatch={dispatch} />}
        {/* Only `state` in: what this screen lists is read through a command, and the one thing
            it needs from the core is the news that a server changed — a death in particular, which
            nobody clicked and which nothing else would bring to the screen. */}
        {state.screen === "mcp" && <Mcp state={state} />}
        {/* No props either, and for a second reason besides Settings': what the voice session
            says arrives on its own Tauri event rather than through the core bus, because a
            microphone is not something the node did about its connection to Attacca. */}
        {state.screen === "voice" && <Voice />}
        {/* Mounted for the life of the window and hidden when another screen is showing: the
            turns live nowhere else, and what is said while another screen is open still counts. */}
        <Conversation hidden={state.screen !== "conversation"} />
        {/* No props: what this screen shows is read off the machine through a command, not
            folded into core state, because nothing outside it needs the answer. */}
        {state.screen === "settings" && <Settings />}
        {overlays}
      </div>
    );
  }
  return (
    <main className="flex h-full items-center justify-center">
      <p className="text-muted-foreground">Starting…</p>
      {overlays}
    </main>
  );
}
