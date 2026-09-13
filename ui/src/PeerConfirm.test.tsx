import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Mocked at the module boundary rather than stubbed inside the component: `invoke` is the whole
// of what this screen can do to the machine, so a test that let a real one through would either
// hang on a bridge that is not there or, worse, be measuring something other than the button.
//
// Through `vi.hoisted` because `vi.mock` is lifted above the imports, and a factory that closed
// over an ordinary `const` would be reaching for a binding that has not been evaluated yet.
const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn<(command: string, args?: unknown) => Promise<unknown>>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { PeerConfirm } from "./PeerConfirm";
import type { PeerQuestion } from "./state";

const FIRST: PeerQuestion = {
  id: 1,
  label: "laptop",
  fingerprint: "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8",
};

// Deliberately the same length as the first, because that is the shape of the attack: a label a
// sender chose so that Approve does not move a single pixel when the question underneath it is
// replaced, and a fingerprint nobody has read.
const SECOND: PeerQuestion = {
  id: 2,
  label: "laptOp",
  fingerprint: "1B44 D0C9 7E62 AA30 F518 6B27 C94D 03E1",
};

// The lockout in PeerConfirm.tsx. Written out rather than imported because it is not exported,
// and a test that imported it would pass for any value including zero — the number is the
// subject, so it has to be named here as well.
const SWAP_LOCKOUT_MS = 750;

// And the recheck interval, for the one test that has to let a question end by itself.
const RECHECK_MS = 1000;

// One function for every render, because `useReducer`'s `dispatch` is one function for the life of
// the component and this screen's effect lists it as a dependency. A fresh arrow per render would
// re-run that effect on every rerender and make the lockout look like it re-arms on its own —
// which it does not do in the app, and must not appear to do here.
const dispatch = () => {};

function approve(): HTMLButtonElement {
  return screen.getByRole("button", { name: "Approve" }) as HTMLButtonElement;
}

function refuse(): HTMLButtonElement {
  return screen.getByRole("button", { name: "Refuse" }) as HTMLButtonElement;
}

// `setTimeout` and `setInterval` are both this screen's, and both have to be driven by hand.
// Wrapped in `act` so React has flushed the state change the timer caused before anything is
// asserted about the DOM.
async function wait(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

describe("PeerConfirm", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invoke.mockReset();
    // The recheck's answer: the question it is showing is still the one waiting. Anything else
    // would end the question mid-test for reasons that are not what is being measured.
    invoke.mockImplementation((command: string) => {
      if (command === "pending_peer") return Promise.resolve(FIRST);
      if (command === "answer_peer") return Promise.resolve(true);
      return Promise.resolve(null);
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("refuses to answer for a beat when a question arrives", async () => {
    render(<PeerConfirm question={FIRST} dispatch={dispatch} />);

    // Even the first question is locked. A window raised *by* the question puts it in front of
    // somebody whose hand may already be on the mouse for another reason entirely.
    expect(approve().disabled).toBe(true);
    expect(refuse().disabled).toBe(true);

    await wait(SWAP_LOCKOUT_MS);

    expect(approve().disabled).toBe(false);
    expect(refuse().disabled).toBe(false);
  });

  it("kills both answers again the moment the question is replaced", async () => {
    // **The failure this exists for.** `Pending::ask` frees the slot as soon as a question ends,
    // so a sender that retries lands a second question here — and App.tsx renders this component
    // without a `key`, so React reuses it and the new label, the new fingerprint and the effect
    // that re-arms the buttons all land on one commit. A person mid-click on the first question
    // would finish that click on the second, pinning a key nobody read, under a name the other
    // side chose to be exactly as wide as the one they did read.
    const { rerender } = render(<PeerConfirm question={FIRST} dispatch={dispatch} />);
    await wait(SWAP_LOCKOUT_MS);
    expect(approve().disabled).toBe(false);

    rerender(<PeerConfirm question={SECOND} dispatch={dispatch} />);

    // The new question is on the screen — this is a swap, not an unmount — and neither answer is
    // live on it.
    expect(screen.getByText(SECOND.fingerprint)).toBeTruthy();
    expect(approve().disabled).toBe(true);
    expect(refuse().disabled).toBe(true);

    // And a click decided before the swap reaches nothing. `click()` on a disabled button fires
    // no event at all, which is the mechanism; the assertion is that nothing was sent either way.
    approve().click();
    expect(invoke).not.toHaveBeenCalledWith("answer_peer", expect.anything());

    await wait(SWAP_LOCKOUT_MS);
    expect(approve().disabled).toBe(false);
  });

  it("drops the last question's ending when a new one takes the screen", async () => {
    // The same seam one state further on. When a question ends with nobody answering, this screen
    // swaps both buttons for a sentence and a Close — and Close now names the question it is
    // closing. A reset done from an effect would leave that Close button over the *new* question
    // for a painted frame, where one click would clear a live question out of the window and stop
    // the poll that would have brought it back.
    invoke.mockImplementation(() => Promise.resolve(null));

    const { rerender } = render(<PeerConfirm question={FIRST} dispatch={dispatch} />);
    await wait(RECHECK_MS);

    expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Approve" })).toBe(null);

    rerender(<PeerConfirm question={SECOND} dispatch={dispatch} />);

    expect(screen.queryByRole("button", { name: "Close" })).toBe(null);
    expect(approve().disabled).toBe(true);
  });

  it("lets the same question through without restarting the lockout", async () => {
    // The forwarder resends a question whenever the window falls behind, and the reducer replaces
    // it with an equal value. Re-arming the lockout on each of those would take the buttons away
    // from somebody who has been reading the same fingerprint for ten seconds — a guard that
    // fires on the ordinary case is one people learn to wait out.
    const { rerender } = render(<PeerConfirm question={FIRST} dispatch={dispatch} />);
    await wait(SWAP_LOCKOUT_MS);

    rerender(<PeerConfirm question={{ ...FIRST }} dispatch={dispatch} />);

    expect(approve().disabled).toBe(false);
  });
});
