import { useEffect, useReducer } from "react";
import { Onboarding } from "./Onboarding";
import { Status } from "./Status";
import { fetchLatestEvent, initialState, reduce, subscribe } from "./state";

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
  if (state.screen === "status") return <Status state={state} />;
  return (
    <main className="screen">
      <p className="muted">Starting.</p>
    </main>
  );
}
