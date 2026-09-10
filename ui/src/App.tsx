import { useEffect, useReducer } from "react";
import { Onboarding } from "./Onboarding";
import { Status } from "./Status";
import { initialState, reduce, subscribe } from "./state";

export function App() {
  const [state, dispatch] = useReducer(reduce, initialState);

  useEffect(() => {
    // subscribe resolves to an unlisten function; React may run this effect twice in StrictMode,
    // so the cleanup has to cover a subscription that is still being set up.
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void subscribe(dispatch).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
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
