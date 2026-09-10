import { useEffect, useReducer } from "react";
import { Onboarding } from "./Onboarding";
import { Status } from "./Status";
import { Tools } from "./Tools";
import {
  fetchLatestEvent,
  initialState,
  reduce,
  subscribe,
  type Screen,
  type Tab,
} from "./state";

// The whole of navigation. Two screens, named once, so the sidebar and the branch below cannot
// disagree about what exists.
const TABS: { id: Tab; label: string }[] = [
  { id: "status", label: "Status" },
  { id: "tools", label: "Tools" },
];

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
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  if (state.screen === "onboarding") return <Onboarding state={state} />;
  // The sidebar appears only once this machine is enrolled: before that there is nothing to
  // navigate to, and offering a choice of screens to someone who has not authorized the computer
  // yet is offering them a way to miss the one thing they have to do.
  if (state.screen === "status" || state.screen === "tools") {
    return (
      <div className="shell">
        <Sidebar screen={state.screen} onNavigate={(to) => dispatch({ kind: "navigate", to })} />
        {state.screen === "status" ? (
          <Status state={state} />
        ) : (
          <Tools state={state} dispatch={dispatch} />
        )}
      </div>
    );
  }
  return (
    <main className="screen">
      <p className="muted">Starting.</p>
    </main>
  );
}
