import { describe, expect, it } from "vitest";
import { initialState, reduce, type PeerQuestion, type State } from "./state";

const FIRST: PeerQuestion = {
  id: 1,
  label: "laptop",
  fingerprint: "9F2A 41C7 0E83 BB15 6D04 A97E 22C1 5FB8",
};

const SECOND: PeerQuestion = {
  id: 2,
  label: "desktop",
  fingerprint: "1B44 D0C9 7E62 AA30 F518 6B27 C94D 03E1",
};

function showing(question: PeerQuestion): State {
  return { ...initialState, screen: "status", question };
}

describe("peerQuestionEnded", () => {
  it("clears the question it names", () => {
    expect(reduce(showing(FIRST), { kind: "peerQuestionEnded", id: FIRST.id }).question).toBe(null);
  });

  it("leaves a different question alone", () => {
    // **The race this guard exists for.** `Pending::answer` empties the slot before the reply to
    // `answer_peer` leaves the process, so a second `send_to` can install its question and publish
    // `needsPeerApproval` in that gap — and that event can reach this reducer *ahead of* the
    // reply to the first answer. Clearing on the reply alone would blank the second question,
    // unmount PeerConfirm.tsx and stop the poll that would otherwise have brought it back, leaving
    // a live question holding the slot unseen for its whole 45 seconds while somebody sat at the
    // screen — and its caller told `peer_not_confirmed` for a question nobody was ever shown.
    const state = showing(SECOND);
    const after = reduce(state, { kind: "peerQuestionEnded", id: FIRST.id });

    expect(after.question).toEqual(SECOND);
    // The very same state object, not an equal one: this arm has to be a no-op rather than a
    // rebuild that happens to look the same.
    expect(after).toBe(state);
  });

  it("does nothing when no question is showing", () => {
    const state = { ...initialState, screen: "status" as const };

    expect(reduce(state, { kind: "peerQuestionEnded", id: FIRST.id })).toBe(state);
  });

  it("leaves the screen underneath exactly where it was", () => {
    // The whole reason the question lives beside `screen` rather than in it: answering hands a
    // person back to the tab they were on.
    const state = { ...initialState, screen: "tools" as const, question: FIRST };

    expect(reduce(state, { kind: "peerQuestionEnded", id: FIRST.id }).screen).toBe("tools");
  });
});
