import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

// `vi.hoisted` for the reason Voice.test.tsx gives: `vi.mock`'s factory is lifted above the
// imports, so it cannot close over an ordinary `const`.
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

import { Debug } from "./Debug";
import type { Trace } from "./state";

// Let the subscription's promise resolve before anything is emitted at it.
async function opened() {
  render(<Debug />);
  await act(async () => {
    await Promise.resolve();
  });
}

async function show(steps: Trace[]) {
  await opened();
  await act(async () => {
    steps.forEach(emit);
  });
  return document.body.textContent ?? "";
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("the Debug screen", () => {
  it("says nothing has happened rather than showing an empty list", async () => {
    await opened();
    // An empty log and a screen that is not receiving look identical, so the empty state names
    // the first thing to check — which on Wayland is the thing that is usually wrong.
    expect(screen.getByText(/nothing yet/i)).toBeTruthy();
    expect(document.body.textContent ?? "").toMatch(/key down/i);
  });

  it("renders every step as its own line, and no two of them read the same", async () => {
    // The whole value of this screen is telling six failure points apart at a glance. A step
    // that rendered like its neighbour would send somebody to the wrong half of the pipeline.
    const steps: Trace[] = [
      { step: "key", down: true },
      { step: "key", down: false },
      { step: "recording", started: true },
      { step: "recording", started: false },
      { step: "recorded", seconds: 2.5, speechSeconds: 1.8, kept: true },
      { step: "recorded", seconds: 0.4, speechSeconds: 0.1, kept: false },
      { step: "transcribing", seconds: 2.1 },
      { step: "transcribed", text: "what is the time", tookMs: 880 },
      { step: "transcribed", text: "", tookMs: 120 },
      { step: "sent", text: "what is the time" },
      { step: "sendFailed", reason: "the connection is gone" },
      { step: "delta", kind: "Assistant", text: "It is four." },
      { step: "fragment", text: "It is four." },
      { step: "synthesised", text: "It is four.", seconds: 1.4, tookMs: 2100 },
      { step: "queued", text: "It is four.", atSample: 61740, samples: 61740 },
      { step: "playing", atSample: 30870 },
      { step: "dropped" },
      { step: "spoke" },
      { step: "interrupted", heard: 2, unheard: 1 },
      { step: "failed", reason: "the microphone was unplugged" },
    ];

    await show(steps);
    const rendered = Array.from(document.querySelectorAll(".trace li")).map(
      (line) => line.textContent?.replace(/^\d\d:\d\d:\d\d\.\d\d\d\s*/, "") ?? "",
    );

    expect(rendered).toHaveLength(steps.length);
    expect(new Set(rendered).size).toBe(steps.length);
    for (const line of rendered) expect(line.length).toBeGreaterThan(3);
  });

  it("tells a key that never arrived apart from one nothing was listening to", async () => {
    // The blind spot this screen shipped with. `Trace::Key` used to be published by the
    // session, which only exists while listening is on — so a key press on a machine whose
    // model had not been downloaded produced *nothing at all*, exactly like a key the
    // compositor never sent. Those two send somebody to opposite ends of the problem.
    const arrived = await show([
      { step: "key", down: true },
      { step: "key", down: false },
    ]);
    expect(arrived).toMatch(/key down/i);
    expect(arrived).not.toMatch(/recording started/i);

    cleanup();
    const listened = await show([
      { step: "key", down: true },
      { step: "recording", started: true },
    ]);
    expect(listened).toMatch(/key down/i);
    expect(listened).toMatch(/recording started/i);
  });

  it("names both reasons for silence when nothing has arrived at all", async () => {
    // An empty log is the state somebody is most likely to be looking at when they ask for
    // help, so it carries the two questions rather than making them ask.
    await opened();
    const page = document.body.textContent ?? "";
    expect(page).toMatch(/not reaching zyris/i);
    expect(page).toMatch(/nothing was listening/i);
    expect(page).toMatch(/model has not been downloaded/i);
  });

  it("says a turn the silence rule threw away was thrown away", async () => {
    // The failure that otherwise looks exactly like whisper returning nothing: there was not
    // enough speech in the turn, so the model never saw it. Whoever is watching needs to know
    // to speak for longer, not to check the model.
    const page = await show([{ step: "recorded", seconds: 0.4, speechSeconds: 0.1, kept: false }]);
    expect(page).toMatch(/below the floor/i);
    expect(page).toMatch(/whisper never saw it/i);
  });

  it("separates what the agent wrote from what is spoken", async () => {
    // The filter drops code fences, asides and URLs, so a fragment is not the delta. Seeing
    // both is the only way to tell "the filter ate it" from "the agent never said it".
    const page = await show([
      { step: "delta", kind: "Assistant", text: "Run `ls` in /tmp." },
      { step: "fragment", text: "Run code in." },
    ]);
    expect(page).toMatch(/the agent wrote/i);
    expect(page).toMatch(/to be spoken/i);
    expect(page).toContain("Run `ls` in /tmp.");
    expect(page).toContain("Run code in.");
  });

  it("marks the two halves of the pipeline and the failures apart", async () => {
    // Colour is the second signal and never the only one — every line says what it is in
    // words — but the classes are what makes the log scannable, so they are pinned.
    await show([
      { step: "key", down: true },
      { step: "fragment", text: "Yes." },
      { step: "sendFailed", reason: "the connection is gone" },
    ]);
    expect(document.querySelectorAll(".trace-in")).toHaveLength(1);
    expect(document.querySelectorAll(".trace-out")).toHaveLength(1);
    expect(document.querySelectorAll(".trace-bad")).toHaveLength(1);
  });
});
