import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { listen, emit } = vi.hoisted(() => {
  const handlers: ((message: { payload: unknown }) => void)[] = [];
  return {
    listen: vi.fn((_name: string, handler: (message: { payload: unknown }) => void) => {
      handlers.push(handler);
      return Promise.resolve(() => {
        handlers.splice(handlers.indexOf(handler), 1);
      });
    }),
    emit: (payload: unknown) => handlers.forEach((handler) => handler({ payload })),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { Conversation } from "./Conversation";
import type { Trace } from "./state";

async function show(steps: Trace[]) {
  render(<Conversation />);
  await act(async () => {
    await Promise.resolve();
  });
  await act(async () => {
    steps.forEach(emit);
  });
  return document.body.textContent ?? "";
}

// One whole exchange, which most tests below start from and then cut short.
const AN_ANSWER: Trace[] = [
  { step: "key", down: true },
  { step: "recording", started: true },
  { step: "key", down: false },
  { step: "recording", started: false },
  { step: "recorded", seconds: 2.5, speechSeconds: 1.8, kept: true },
  { step: "transcribing", seconds: 2.1 },
  { step: "transcribed", text: "what is the time", tookMs: 880 },
  { step: "sent", text: "what is the time" },
  { step: "delta", kind: "Assistant", text: "It is four. " },
  { step: "fragment", text: "It is four." },
  { step: "synthesised", text: "It is four.", seconds: 1.4, tookMs: 2100 },
  { step: "queued", text: "It is four.", atSample: 0, samples: 61740 },
];

function bars(): number[] {
  return Array.from(document.querySelectorAll<HTMLElement>(".sentence-bar > span")).map((bar) =>
    Number.parseInt(bar.style.width, 10),
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("the Conversation screen", () => {
  it("says there is no wake word rather than leaving somebody waiting for one", async () => {
    render(<Conversation />);
    await act(async () => {
      await Promise.resolve();
    });
    // Nothing reads the takes recorded on the Voice tab. A blank screen with a microphone
    // switched on is exactly the state somebody sits in waiting to be heard.
    expect(screen.getByText(/no wake word/i)).toBeTruthy();
    expect(document.body.textContent ?? "").toMatch(/only way to start a turn/i);
  });

  it("shows both sides of one exchange", async () => {
    const page = await show(AN_ANSWER);
    expect(page).toContain("what is the time");
    expect(page).toContain("It is four.");
    expect(page).toMatch(/You/);
    expect(page).toMatch(/Attacca/);
  });

  it("does not pretend to transcribe as you speak", async () => {
    // Whisper answers once per look and re-reads the whole recording each time, so there is
    // no growing transcript. Between the key coming up and the answer landing there is
    // nothing to show, and the screen says so rather than leaving the last look sitting there
    // looking settled.
    const page = await show(AN_ANSWER.slice(0, 5));
    expect(page).toMatch(/writing it down/i);
    expect(page).not.toContain("what is the time");
  });

  it("shows what has been heard so far while the key is still down", async () => {
    // The half of the request that needed the session to look at a turn in progress. It is
    // marked as provisional because it is: the next look re-reads the recording from the
    // start and may revise it.
    const page = await show([
      { step: "recording", started: true },
      { step: "hearing", text: "what is the", seconds: 1.5 },
    ]);
    expect(page).toContain("what is the");
    expect(page).toMatch(/still listening/i);
  });

  it("replaces a look rather than appending to it", async () => {
    // Whisper revises. Concatenating would produce "what iswhat is the time" and would read
    // as the model stuttering rather than as it changing its mind.
    const page = await show([
      { step: "recording", started: true },
      { step: "hearing", text: "what is", seconds: 1.5 },
      { step: "hearing", text: "what is the time", seconds: 3.0 },
    ]);
    expect(page).toContain("what is the time");
    expect(page).not.toMatch(/what iswhat/);
  });

  it("drops a look once the key has come up", async () => {
    // The turn is no longer being recorded, so a look is stale by definition. Leaving it on
    // screen beside "writing it down" would be two answers to one question.
    const page = await show([
      { step: "recording", started: true },
      { step: "hearing", text: "what is", seconds: 1.5 },
      { step: "recording", started: false },
    ]);
    expect(page).toMatch(/writing it down/i);
    expect(page).not.toContain("what is");
  });

  it("fills a sentence as it is actually played, not as time passes", async () => {
    await show(AN_ANSWER);
    expect(bars()).toEqual([0]);

    await act(async () => {
      emit({ step: "playing", atSample: 30870 } satisfies Trace);
    });
    expect(bars()).toEqual([50]);
    expect(document.body.textContent ?? "").toMatch(/50% of 1\.4s/);

    await act(async () => {
      emit({ step: "playing", atSample: 61740 } satisfies Trace);
    });
    expect(bars()).toEqual([100]);
    expect(document.body.textContent ?? "").toMatch(/heard/i);
  });

  it("tells four states of one sentence apart", async () => {
    // Waiting for the voice, made, queued, heard. Collapsing any two of them would hide the
    // thing this screen was asked for: how much has been turned into audio, against how much
    // has actually been played.
    const said: string[] = [];
    const steps: Trace[] = [
      { step: "fragment", text: "One." },
      { step: "synthesised", text: "One.", seconds: 1.0, tookMs: 900 },
      { step: "queued", text: "One.", atSample: 0, samples: 44100 },
      { step: "playing", atSample: 44100 },
    ];
    for (let upto = 1; upto <= steps.length; upto += 1) {
      await show(steps.slice(0, upto));
      said.push(document.querySelector(".sentence-state")?.textContent ?? "");
      cleanup();
    }
    expect(new Set(said).size).toBe(4);
    expect(said[0]).toMatch(/waiting for the voice/i);
    expect(said[3]).toMatch(/heard/i);
  });

  it("marks a sentence finished after the key went down as never heard", async () => {
    // It was made and nobody will hear it. Showing it as queued would have the screen claim
    // audio that was thrown away.
    const page = await show([
      { step: "fragment", text: "One." },
      { step: "synthesised", text: "One.", seconds: 1.0, tookMs: 900 },
      { step: "dropped" },
    ]);
    expect(page).toMatch(/not heard/i);
  });

  it("does not read the agent's working-out as part of its answer", async () => {
    // `Reasoning` deltas are the agent thinking, and `speak::Filter` already refuses to say
    // them aloud. Putting them in the conversation would contradict what is being spoken.
    const page = await show([
      { step: "delta", kind: "Reasoning", text: "The user wants the time." },
      { step: "delta", kind: "Assistant", text: "It is four." },
    ]);
    expect(page).toContain("It is four.");
    expect(page).not.toContain("The user wants the time.");
  });

  it("says a turn the silence rule threw away was never sent", async () => {
    const page = await show([
      { step: "recording", started: true },
      { step: "recording", started: false },
      { step: "recorded", seconds: 0.4, speechSeconds: 0.1, kept: false },
    ]);
    expect(page).toMatch(/nothing was sent/i);
    expect(page).toMatch(/below the floor/i);
  });

  it("says a transcript that did not reach Attacca did not reach it", async () => {
    // The gap that existed until the loop was joined: without this the turn would sit there
    // looking sent, and the missing answer would read as the agent being slow.
    const page = await show([
      { step: "recording", started: true },
      { step: "recording", started: false },
      { step: "transcribed", text: "hello", tookMs: 400 },
      { step: "sendFailed", reason: "the connection is gone" },
    ]);
    expect(page).toMatch(/nothing was sent/i);
    expect(page).toContain("the connection is gone");
  });

  it("says the rest was not read aloud after an interruption", async () => {
    const page = await show([...AN_ANSWER, { step: "interrupted", heard: 1, unheard: 2 }]);
    expect(page).toMatch(/not read aloud/i);
    // And does not claim the agent's record is truncated, which it is not.
    expect(page).toMatch(/record of the answer is whole/i);
  });

  it("attributes a repeated sentence to the one being worked on", async () => {
    // An answer may say the same thing twice. Matching from the start would put the second
    // sentence's audio onto the first and leave the second looking stuck forever.
    await show([
      { step: "fragment", text: "Yes." },
      { step: "synthesised", text: "Yes.", seconds: 1.0, tookMs: 900 },
      { step: "queued", text: "Yes.", atSample: 0, samples: 44100 },
      { step: "fragment", text: "Yes." },
      { step: "synthesised", text: "Yes.", seconds: 1.0, tookMs: 900 },
      { step: "queued", text: "Yes.", atSample: 44100, samples: 44100 },
      { step: "playing", atSample: 66150 },
    ]);
    expect(bars()).toEqual([100, 50]);
  });
});
