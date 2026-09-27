import { describe, expect, it } from "vitest";
import type { Trace } from "../state";
import { fold, type Action, type AgentTurn, type Turn, type YouTurn } from "./fold";

function run(...steps: Action[]): Turn[] {
  return steps.reduce<Turn[]>(fold, []);
}

function last<T extends Turn["who"]>(turns: Turn[], who: T): Extract<Turn, { who: T }> {
  const found = [...turns].reverse().find((turn) => turn.who === who);
  if (!found) throw new Error(`no ${who} turn`);
  return found as Extract<Turn, { who: T }>;
}

const you = (turns: Turn[]): YouTurn => last(turns, "you");
const agent = (turns: Turn[]): AgentTurn => last(turns, "agent");

const assistant = (text: string): Trace => ({ step: "delta", kind: "Assistant", text });

describe("what you say", () => {
  it("is being transcribed from the key going down until the text is back", () => {
    let turns = run({ step: "recording", started: true });
    expect(you(turns).state).toBe("recording");

    turns = fold(turns, { step: "hearing", text: "Can you", seconds: 1.5 });
    expect(you(turns).sofar).toBe("Can you");

    turns = fold(turns, { step: "recording", started: false });
    expect(you(turns).state).toBe("transcribing");

    turns = fold(turns, { step: "transcribed", text: "Can you check?", tookMs: 900 });
    expect(you(turns)).toMatchObject({ state: "said", text: "Can you check?" });

    turns = fold(turns, { step: "sent", text: "Can you check?" });
    expect(you(turns).state).toBe("sent");
  });

  it("replaces the guess rather than appending to it — whisper re-reads the whole turn", () => {
    const turns = run(
      { step: "recording", started: true },
      { step: "hearing", text: "Can you", seconds: 1.5 },
      { step: "hearing", text: "Can you check why", seconds: 3 },
    );
    expect(you(turns).sofar).toBe("Can you check why");
  });

  it("is transcribing when a turn the wake word opened ends on silence", () => {
    // That turn ends with no `recording{started:false}`: the next thing is the transcription.
    const turns = run({ step: "recording", started: true }, { step: "transcribing", seconds: 2 });
    expect(you(turns).state).toBe("transcribing");
  });

  it("is lost, with the reason, when nothing was said or it did not arrive", () => {
    expect(
      you(run({ step: "recording", started: true }, { step: "transcribed", text: "", tookMs: 1 })),
    ).toMatchObject({ state: "lost" });
    expect(
      you(
        run(
          { step: "recording", started: true },
          { step: "recorded", seconds: 2, speechSeconds: 0.1, kept: false },
        ),
      ).state,
    ).toBe("lost");
    const failed = run(
      { step: "recording", started: true },
      { step: "transcribed", text: "Hello", tookMs: 1 },
      { step: "sendFailed", reason: "the connection is down" },
    );
    expect(you(failed)).toMatchObject({ state: "lost", detail: "the connection is down" });
  });

  it("shows a typed message as not sent until it is, then sent", () => {
    let turns = run({ step: "typed", text: "Hello there" });
    expect(you(turns)).toMatchObject({ state: "said", text: "Hello there" });
    turns = fold(turns, { step: "sent", text: "Hello there" });
    expect(you(turns).state).toBe("sent");
  });

  it("marks a typed message lost when it did not go", () => {
    const turns = run(
      { step: "typed", text: "Hello" },
      { step: "typedFailed", text: "Hello", reason: "not connected" },
    );
    expect(you(turns)).toMatchObject({ state: "lost", detail: "not connected" });
  });

  it("matches a sent message to its own turn, not merely the last one", () => {
    const turns = run(
      { step: "typed", text: "First" },
      { step: "typed", text: "Second" },
      { step: "sent", text: "First" },
    );
    const yours = turns.filter((turn): turn is YouTurn => turn.who === "you");
    expect(yours.map((turn) => turn.state)).toEqual(["sent", "said"]);
  });

  it("adds a message sent from here that the window never saw begin", () => {
    const turns = run({ step: "sent", text: "From somewhere" });
    expect(you(turns)).toMatchObject({ state: "sent", text: "From somewhere" });
  });
});

describe("what the agent says", () => {
  it("opens an answer, streams its text, and closes it", () => {
    let turns = run({ step: "answering", aloud: true }, assistant("Sure — "), assistant("done."));
    expect(agent(turns)).toMatchObject({ text: "Sure — done.", aloud: true, writing: true });
    turns = fold(turns, { step: "answered" });
    expect(agent(turns).writing).toBe(false);
  });

  it("does not show reasoning", () => {
    const turns = run(
      { step: "answering", aloud: false },
      { step: "delta", kind: "Reasoning", text: "thinking" },
      assistant("Yes."),
    );
    expect(agent(turns).text).toBe("Yes.");
  });

  it("starts a new answer for each answering, even with nothing said in between", () => {
    const turns = run(
      { step: "answering", aloud: false },
      assistant("One."),
      { step: "answered" },
      { step: "answering", aloud: false },
      assistant("Two."),
    );
    expect(turns.filter((turn) => turn.who === "agent")).toHaveLength(2);
  });

  it("follows each sentence through the voice and the speaker", () => {
    let turns = run(
      { step: "answering", aloud: true },
      assistant("Yes. No."),
      { step: "fragment", text: "Yes." },
    );
    expect(agent(turns).sentences[0]).toMatchObject({ text: "Yes.", seconds: null, at: null });
    turns = fold(turns, { step: "synthesised", text: "Yes.", seconds: 0.5, tookMs: 300 });
    turns = fold(turns, { step: "queued", text: "Yes.", atSample: 1000, samples: 500 });
    expect(agent(turns).sentences[0]).toMatchObject({ seconds: 0.5, at: 1000, samples: 500 });
    turns = fold(turns, { step: "playing", atSample: 1200 });
    expect(agent(turns).played).toBe(1200);
  });

  it("marks the newest unqueued sentence dropped", () => {
    const turns = run(
      { step: "answering", aloud: true },
      { step: "fragment", text: "One." },
      { step: "fragment", text: "Two." },
      { step: "dropped" },
    );
    expect(agent(turns).sentences.map((s) => s.dropped)).toEqual([false, true]);
  });

  it("is settled once speech ends or is cut off, or the next answer begins", () => {
    expect(agent(run({ step: "answering", aloud: true }, { step: "spoke" })).settled).toBe(true);
    expect(
      agent(run({ step: "answering", aloud: true }, { step: "interrupted", heard: 1, unheard: 2 })),
    ).toMatchObject({ settled: true, interrupted: true });
    const followed = run(
      { step: "answering", aloud: true },
      assistant("Hi."),
      { step: "answering", aloud: true },
    );
    const answers = followed.filter((t): t is AgentTurn => t.who === "agent");
    expect(answers.map((a) => a.settled)).toEqual([true, false]);
  });

  it("keeps filling an answer that is still streaming when something is typed under it", () => {
    const turns = run(
      { step: "answering", aloud: true },
      assistant("First part."),
      { step: "typed", text: "Wait" },
      assistant(" Second part."),
      { step: "fragment", text: "Second part." },
      { step: "playing", atSample: 10 },
    );
    expect(turns.map((t) => t.who)).toEqual(["agent", "you"]);
    expect(agent(turns)).toMatchObject({ text: "First part. Second part.", played: 10 });
    expect(agent(turns).sentences).toHaveLength(1);
  });

  it("gives each answer its own playback position", () => {
    // A speaker reopened counts from zero again, so a position from an older answer must not
    // make a new one look already heard.
    const turns = run(
      { step: "answering", aloud: true },
      { step: "playing", atSample: 90_000 },
      { step: "answering", aloud: true },
    );
    expect(agent(turns).played).toBe(-1);
  });
});

describe("problems", () => {
  it("is said on its own line when no turn of yours is waiting on it", () => {
    const turns = run({ step: "failed", reason: "The agent did not answer." });
    expect(turns).toEqual([{ who: "problem", reason: "The agent did not answer." }]);
  });

  it("ends a turn of yours that was still being transcribed", () => {
    const turns = run({ step: "recording", started: true }, { step: "failed", reason: "no mic" });
    expect(you(turns)).toMatchObject({ state: "lost", detail: "no mic" });
  });
});
