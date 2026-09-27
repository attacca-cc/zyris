import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Every channel the screen listens on, by name, so a test can speak on the one it means.
const { listen, emit } = vi.hoisted(() => {
  const handlers = new Map<string, ((message: { payload: unknown }) => void)[]>();
  return {
    listen: vi.fn((name: string, handler: (message: { payload: unknown }) => void) => {
      handlers.set(name, [...(handlers.get(name) ?? []), handler]);
      return Promise.resolve(() => {
        handlers.set(name, (handlers.get(name) ?? []).filter((h) => h !== handler));
      });
    }),
    emit: (name: string, payload: unknown) =>
      (handlers.get(name) ?? []).forEach((handler) => handler({ payload })),
  };
});
vi.mock("@tauri-apps/api/event", () => ({ listen }));

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { Conversation } from "./Conversation";
import type { Trace } from "./state";

// A voice state with listening on, a key bound and reading aloud on.
function voiceScreen(change: { readAloud?: boolean; listening?: boolean } = {}) {
  return {
    voice: {
      support: { state: "ready" },
      listening: change.listening === false ? { state: "off" } : { state: "on", device: "mic" },
      speaking: { state: "session", id: "s1" },
      readAloud: change.readAloud ?? true,
    },
    hotkey: { state: "working", trigger: "<Control>space", releaseConfirmed: true },
  };
}

function answering(command: string, args?: unknown): Promise<unknown> {
  switch (command) {
    case "voice_state":
      return Promise.resolve(voiceScreen());
    case "set_read_aloud":
      return Promise.resolve(voiceScreen({ readAloud: (args as { readAloud: boolean }).readAloud }));
    case "conversation_sessions":
      return Promise.reject("this machine is not connected to Attacca yet");
    default:
      return Promise.resolve(null);
  }
}

async function open() {
  render(<Conversation hidden={false} />);
  await act(async () => {
    await Promise.resolve();
  });
}

async function steps(...trace: Trace[]) {
  await act(async () => {
    trace.forEach((step) => emit("voice-trace", step));
  });
}

function tones(): [string, string][] {
  return Array.from(document.querySelectorAll<HTMLElement>("[data-tone]")).map((span) => [
    span.textContent ?? "",
    span.dataset.tone ?? "",
  ]);
}

beforeEach(() => invoke.mockImplementation(answering));
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("the Conversation screen", () => {
  it("opens on the messages the session already holds, before anything said now", async () => {
    invoke.mockImplementation((command: string, args?: unknown) =>
      command === "conversation_history"
        ? Promise.resolve({
            session: "s1",
            lines: [
              { who: "you", text: "들리니?" },
              { who: "agent", text: "네, 잘 들려요." },
            ],
          })
        : answering(command, args),
    );
    await open();
    await screen.findByText("들리니?");
    await steps({ step: "sent", text: "지금 말한 것" });

    const said = Array.from(document.querySelectorAll("[data-turn]")).map((turn) => turn.textContent ?? "");
    expect(said[0]).toMatch(/들리니\?/);
    expect(said[1]).toMatch(/네, 잘 들려요\./);
    expect(said[said.length - 1]).toMatch(/지금 말한 것/);
  });

  it("says what the agent is doing while it works, and keeps the tool count after", async () => {
    await open();
    await steps(
      { step: "sent", text: "찾아줘" },
      { step: "answering", aloud: false },
      { step: "working", title: "코드를 찾는 중" },
      { step: "tool", name: "shell" },
    );
    expect(screen.getByText("코드를 찾는 중 · 1 tool")).toBeTruthy();
    await steps({ step: "delta", kind: "Assistant", text: "찾았어요." }, { step: "answered" });
    expect(screen.queryByText("코드를 찾는 중 · 1 tool")).toBeNull();
    expect(screen.getByText("Used 1 tool")).toBeTruthy();
  });

  it("invites a first message and names the key", async () => {
    await open();
    expect(screen.getByText("Start a conversation")).toBeTruthy();
    const status = screen.getByRole("status");
    expect(status.textContent).toMatch(/Hold\s*Ctrl\s*Space\s*to talk/);
  });

  it("listens on the trace and the levels, and nothing else", async () => {
    await open();
    const names = listen.mock.calls.map(([name]) => name).sort();
    expect(names).toEqual(["voice-level", "voice-trace"]);
  });

  it("shows what you say grey and italic while it is transcribed, then white once sent", async () => {
    await open();
    await steps({ step: "recording", started: true }, { step: "hearing", text: "what is", seconds: 1.5 });
    const bubble = () => document.querySelector<HTMLElement>('[data-turn="you"] div');
    expect(bubble()?.textContent).toBe("what is");
    expect(bubble()?.className).toMatch(/italic/);
    expect(bubble()?.className).toMatch(/text-muted-foreground/);

    await steps({ step: "recording", started: false }, { step: "transcribed", text: "what is the time", tookMs: 900 });
    expect(bubble()?.className).not.toMatch(/italic/);
    expect(bubble()?.className).toMatch(/text-muted-foreground/);

    await steps({ step: "sent", text: "what is the time" });
    expect(bubble()?.className).toMatch(/text-heading/);
  });

  it("streams the answer and colours each sentence by how far it has got", async () => {
    await open();
    await steps(
      { step: "answering", aloud: true },
      { step: "delta", kind: "Assistant", text: "It is four. It is late" },
    );
    expect(tones()).toEqual([["It is four. It is late", "pending"]]);

    await steps(
      { step: "fragment", text: "It is four." },
      { step: "synthesised", text: "It is four.", seconds: 1, tookMs: 500 },
    );
    expect(tones()).toEqual([
      ["It is four.", "voiced"],
      [" It is late", "pending"],
    ]);

    await steps(
      { step: "queued", text: "It is four.", atSample: 0, samples: 44100 },
      { step: "playing", atSample: 100 },
    );
    expect(tones()[0]).toEqual(["It is four.", "spoken"]);
    expect(screen.getByRole("status").textContent).toMatch(/Speaking/);
  });

  it("shows an answer that is not read aloud as it is", async () => {
    await open();
    await steps({ step: "answering", aloud: false }, { step: "delta", kind: "Assistant", text: "Hello." });
    expect(tones()).toEqual([["Hello.", "plain"]]);
  });

  it("sends a typed message and shows it before it arrives", async () => {
    await open();
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "Hi there" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));

    expect(invoke).toHaveBeenCalledWith("send_conversation_text", { text: "Hi there" });
    const bubble = document.querySelector<HTMLElement>('[data-turn="you"] div');
    expect(bubble?.textContent).toBe("Hi there");
    expect(bubble?.className).toMatch(/text-muted-foreground/);

    await steps({ step: "sent", text: "Hi there" });
    expect(document.querySelector<HTMLElement>('[data-turn="you"] div')?.className).toMatch(/text-heading/);
  });

  it("says when a typed message did not go", async () => {
    invoke.mockImplementation((command: string, args?: unknown) =>
      command === "send_conversation_text" ? Promise.reject("not connected") : answering(command, args),
    );
    await open();
    fireEvent.change(screen.getByRole("textbox", { name: "Message" }), { target: { value: "Hi" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(await screen.findByText(/Not sent — not connected/)).toBeTruthy();
  });

  it("switches reading aloud through the Rust side", async () => {
    await open();
    const toggle = await screen.findByRole("button", { name: /read aloud/i });
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    await act(async () => {
      fireEvent.click(toggle);
    });
    expect(invoke).toHaveBeenCalledWith("set_read_aloud", { readAloud: false });
    expect(screen.getByRole("button", { name: /read aloud/i }).getAttribute("aria-pressed")).toBe("false");
  });

  it("stops speaking on request", async () => {
    await open();
    await steps(
      { step: "answering", aloud: true },
      { step: "fragment", text: "One." },
      { step: "queued", text: "One.", atSample: 0, samples: 10 },
    );
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(invoke).toHaveBeenCalledWith("stop_speaking");
  });

  it("turns the microphone on from the composer", async () => {
    invoke.mockImplementation((command: string, args?: unknown) =>
      command === "voice_state" ? Promise.resolve(voiceScreen({ listening: false })) : answering(command, args),
    );
    await open();
    fireEvent.click(await screen.findByRole("button", { name: "Turn the microphone on" }));
    expect(invoke).toHaveBeenCalledWith("set_voice_listening", { listening: true });
  });
});
