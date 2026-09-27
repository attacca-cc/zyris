import { describe, expect, it } from "vitest";
import type { AgentTurn, Sentence } from "./fold";
import { spans } from "./colors";

function sentence(text: string, change: Partial<Sentence> = {}): Sentence {
  return { text, seconds: null, at: null, samples: null, dropped: false, ...change };
}

function turn(text: string, sentences: Sentence[], change: Partial<AgentTurn> = {}): AgentTurn {
  return {
    who: "agent",
    text,
    sentences,
    aloud: true,
    writing: true,
    settled: false,
    interrupted: false,
    played: -1,
    ...change,
  };
}

const tones = (t: AgentTurn) => spans(t).map((s) => [s.text, s.tone]);

describe("the colour of each part of an answer", () => {
  it("is plain white when the answer is not read aloud", () => {
    expect(tones(turn("Hello there.", [], { aloud: false }))).toEqual([["Hello there.", "plain"]]);
  });

  it("is pending while written and not yet voiced", () => {
    // Adjacent spans with the same tone are merged.
    expect(tones(turn("Sure. The job", [sentence("Sure.")]))).toEqual([["Sure. The job", "pending"]]);
  });

  it("is grey once its audio exists and white from the moment it plays", () => {
    const answer = turn(
      "One. Two. Three.",
      [
        sentence("One.", { seconds: 1, at: 0, samples: 100 }),
        sentence("Two.", { seconds: 1, at: 100, samples: 100 }),
        sentence("Three.", { seconds: 1 }),
      ],
      { played: 150, writing: false },
    );
    expect(tones(answer)).toEqual([
      ["One. Two.", "spoken"],
      [" Three.", "voiced"],
    ]);
  });

  it("is red when the sentence was dropped", () => {
    const answer = turn("One. Two.", [
      sentence("One.", { seconds: 1, at: 0, samples: 10 }),
      sentence("Two.", { dropped: true }),
    ], { played: 20 });
    expect(tones(answer)).toEqual([
      ["One.", "spoken"],
      [" Two.", "filtered"],
    ]);
  });

  it("is red for text the filter took out between two sentences it kept", () => {
    const answer = turn("Run this:\n```\nmake\n```\nThen wait.", [
      sentence("Run this:", { seconds: 1, at: 0, samples: 10 }),
      sentence("Then wait.", { seconds: 1, at: 10, samples: 10 }),
    ], { played: 30, writing: false });
    expect(tones(answer)).toEqual([
      ["Run this:", "spoken"],
      ["\n```\nmake\n```\n", "filtered"],
      ["Then wait.", "spoken"],
    ]);
  });

  it("keeps an unmatched tail pending until the answer settles, then red", () => {
    const text = "Done. See https://example.com";
    const voiced = [sentence("Done.", { seconds: 1, at: 0, samples: 10 })];
    expect(tones(turn(text, voiced, { writing: false, played: 20 }))).toEqual([
      ["Done.", "spoken"],
      [" See https://example.com", "pending"],
    ]);
    expect(tones(turn(text, voiced, { writing: false, settled: true, played: 20 }))).toEqual([
      ["Done.", "spoken"],
      [" See https://example.com", "filtered"],
    ]);
  });

  it("finds a sentence the filter reshaped — markdown taken out, spacing changed", () => {
    const answer = turn("It is **really** done.", [
      sentence("It is really done.", { seconds: 1, at: 0, samples: 10 }),
    ], { played: 5 });
    expect(tones(answer)).toEqual([["It is **really** done.", "spoken"]]);
  });

  it("finds a sentence with an aside taken out of the middle", () => {
    const answer = turn("The build failed (see the log) because of a path.", [
      sentence("The build failed because of a path.", { seconds: 1 }),
    ]);
    expect(tones(answer)).toEqual([["The build failed (see the log) because of a path.", "voiced"]]);
  });

  it("matches a repeated sentence to its own place", () => {
    const answer = turn("Yes. Yes.", [
      sentence("Yes.", { seconds: 1, at: 0, samples: 10 }),
      sentence("Yes.", { seconds: 1 }),
    ], { played: 5 });
    expect(tones(answer)).toEqual([
      ["Yes.", "spoken"],
      [" Yes.", "voiced"],
    ]);
  });

  it("shows fragments even if no text arrived for them", () => {
    const answer = turn("", [sentence("Hello.", { seconds: 1 })]);
    expect(tones(answer)).toEqual([["Hello.", "voiced"]]);
  });
});
