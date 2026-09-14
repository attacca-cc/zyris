import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The whole of what this screen can do to the machine, mocked at the module boundary. Through
// `vi.hoisted` because `vi.mock`'s factory is lifted above the imports, and a factory closing over
// an ordinary `const` reaches for a binding that has not been evaluated yet.
const { invoke, listen, emitVoice } = vi.hoisted(() => {
  const handlers: ((message: { payload: unknown }) => void)[] = [];
  return {
    invoke: vi.fn<(command: string, args?: unknown) => Promise<unknown>>(),
    // The second boundary: the voice session's own Tauri event. `state.ts`'s `subscribeVoice`
    // goes through this, and the screen re-reads itself when a turn fails.
    listen: vi.fn((_name: string, handler: (message: { payload: unknown }) => void) => {
      handlers.push(handler);
      return Promise.resolve(() => {
        handlers.splice(handlers.indexOf(handler), 1);
      });
    }),
    emitVoice: (payload: unknown) => handlers.forEach((handler) => handler({ payload })),
  };
});
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { Voice } from "./Voice";

const MODEL = "/home/ada/.cache/zyris/models/ggml-base.bin";
const TAKES = "/home/ada/.local/share/zyris/wake-word";

type Screen = Record<string, unknown>;

// A machine where everything works, which every test below then breaks one thing of.
function machine(over: {
  support?: unknown;
  listening?: unknown;
  devices?: unknown;
  chosen?: unknown;
  model?: unknown;
  modelEnv?: string | null;
  wake?: unknown;
  hotkey?: unknown;
}): Screen {
  return {
    voice: {
      support: over.support ?? { state: "ready" },
      listening: over.listening ?? { state: "off" },
      devices: over.devices ?? {
        state: "listed",
        devices: [
          {
            id: "alsa_input.builtin",
            name: "Built-in Audio Analog Stereo",
            isDefault: true,
            direction: "input",
          },
          {
            id: "alsa_output.builtin.monitor",
            name: "Built-in Audio Analog Stereo",
            isDefault: false,
            direction: "duplex",
          },
        ],
      },
      chosen: over.chosen ?? { kind: "default" },
      model: over.model ?? { state: "ready", path: MODEL, bytes: 147951465 },
      modelEnv: over.modelEnv ?? null,
      wake: over.wake ?? {
        state: { state: "nothing" },
        dir: TAKES,
        wanted: 5,
        seconds: 5,
        note: "Zyris keeps this recording so a wake word can be added later without asking you to record it again. Nothing listens for it yet, and saving it does not make Zyris respond to it.",
      },
    },
    hotkey: over.hotkey ?? { state: "working", trigger: "Ctrl+Alt+Space" },
  };
}

// `voice_state` and every button answer with the same shape. `then` lets a test say what a button
// answers, which is the case that matters: what the machine did, not what was clicked.
function answers(first: Screen, then?: Screen) {
  invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "voice_state" ? first : (then ?? first)),
  );
}

// Everything a person can actually read, as one string. Deliberately the rendered text and not
// the DOM: a test on class names passes for a screen that paints two states identically and says
// the same words about both.
function readable(): string {
  return document.body.textContent ?? "";
}

describe("Voice", () => {
  beforeEach(() => {
    invoke.mockReset();
    listen.mockClear();
    answers(machine({}));
  });

  afterEach(cleanup);

  it("does not answer anything about the machine while the first read is in flight", async () => {
    // A promise that never settles: the read is in flight, which is a third answer and not a
    // machine with nothing on it.
    invoke.mockImplementation(() => new Promise(() => {}));

    render(<Voice />);

    expect(readable()).not.toMatch(/no microphones/i);
    expect(readable()).not.toMatch(/has not been downloaded/i);
    // And it says what it is doing, rather than sitting blank — a screen with nothing on it also
    // passes the two assertions above.
    expect(readable()).toMatch(/reading what this computer can hear with/i);
  });

  it("says a read that failed outright failed, rather than drawing an empty machine", async () => {
    invoke.mockImplementation(() => Promise.reject("the bridge is not there"));

    render(<Voice />);

    await screen.findByText(/the bridge is not there/);
    expect(readable()).not.toMatch(/no microphones/i);
    expect(readable()).not.toMatch(/nothing has been recorded/i);
  });

  // ------------------------------------------------------------------------------------------
  // The three hotkey answers
  // ------------------------------------------------------------------------------------------

  it("names the key to hold when one is registered", async () => {
    answers(machine({ hotkey: { state: "working", trigger: "Ctrl+Alt+Space" } }));

    render(<Voice />);

    await screen.findByText(/to talk, from any window/i);
    expect(readable()).toMatch(/hold Ctrl\+Alt\+Space to talk/i);
    // And it does not tell somebody with a working key to go and edit a configuration file.
    expect(readable()).not.toMatch(/compositor/i);
    expect(readable()).not.toMatch(/bind = /);
  });

  it("shows the exact line to add when the desktop will only let the person bind the key", async () => {
    answers(
      machine({
        hotkey: {
          state: "needsAKeyBound",
          shortcutId: "push_to_talk",
          desktop: "Hyprland",
          line: "bind = CTRL ALT, space, global, :push_to_talk",
          how: "Hyprland does not let an application choose the key, so Zyris cannot bind Ctrl+Alt+Space for you. Add this line to your compositor configuration if you have not already, and reload it.",
        },
      }),
    );

    render(<Voice />);

    // **The one thing on this screen a person copies.** A mutation deleting it left step 6's
    // screen looking fine, so it is asserted on its own and not through the sentence around it.
    await screen.findByText("bind = CTRL ALT, space, global, :push_to_talk");
    expect(readable()).toMatch(/does not let an application choose the key/i);
    // And the two things this screen must not claim: that Zyris knows whether a key is bound,
    // and that letting go of it is known to work here.
    expect(readable()).toMatch(/cannot tell whether you have bound a key/i);
    expect(readable()).toMatch(/has not been confirmed/i);
  });

  it("shows no line at all for a desktop whose spelling Zyris has not checked", async () => {
    answers(
      machine({
        hotkey: {
          state: "needsAKeyBound",
          shortcutId: "push_to_talk",
          desktop: "GNOME",
          line: null,
          how: "GNOME does not let an application choose the key. Bind one to the global shortcut named push_to_talk in your desktop's keyboard settings; Zyris cannot tell whether you have.",
        },
      }),
    );

    render(<Voice />);

    await screen.findByText(/does not know how GNOME spells that line/i);
    // A guess here is a line somebody pastes into a configuration file. There must not be one.
    expect(readable()).not.toMatch(/bind = /);
    // What is left has to be enough to act on: the name of the shortcut to point a key at.
    expect(readable()).toContain("push_to_talk");
  });

  it("offers no listening switch on a desktop where no key can be registered", async () => {
    answers(
      machine({
        hotkey: {
          state: "unavailable",
          reason: "this desktop's portal has no GlobalShortcuts interface, so no application can register a global key here.",
        },
      }),
    );

    render(<Voice />);

    await screen.findByText(/no GlobalShortcuts interface/i);
    // **The rule this whole project keeps restating**: a control that cannot work is worse than
    // an absent one. Opening a microphone here would open one nothing could ever start a turn on.
    expect(screen.queryByRole("button", { name: /turn on/i })).toBeNull();
    expect(readable()).toMatch(/no push-to-talk key on this desktop/i);
  });

  it("keeps a key that has to be bound by hand apart from one that can never exist", async () => {
    // The same two answers as above, read against each other: the switch is the difference, and a
    // screen that treated `needsAKeyBound` as hopeless would take away the one thing that works.
    answers(
      machine({
        hotkey: {
          state: "needsAKeyBound",
          shortcutId: "push_to_talk",
          desktop: "Hyprland",
          line: "bind = CTRL ALT, space, global, :push_to_talk",
          how: "Hyprland does not let an application choose the key.",
        },
      }),
    );

    render(<Voice />);

    expect(await screen.findByRole("button", { name: /turn on/i })).toBeTruthy();
  });

  // ------------------------------------------------------------------------------------------
  // The switch
  // ------------------------------------------------------------------------------------------

  it("shows what turning listening on actually did, not what was asked for", async () => {
    answers(
      machine({ listening: { state: "off" } }),
      machine({
        listening: {
          state: "failed",
          reason:
            "the speech model has not been downloaded yet, so there is nothing to transcribe with",
        },
        model: { state: "absent", path: MODEL, bytes: 147951465 },
      }),
    );

    render(<Voice />);
    (await screen.findByRole("button", { name: /turn on/i })).click();

    await waitFor(() =>
      expect(readable()).toMatch(/the speech model has not been downloaded yet/i),
    );
    // The badge, matched exactly rather than through `readable()`. **This assertion was a false
    // positive first:** the wake word note further down says "whether or not listening is on",
    // which satisfies /not listening/ on any screen at all, and two mutations of the badge
    // survived behind it.
    expect(screen.getByText("not listening")).toBeTruthy();
  });

  it("gives each of the four listening states its own word", async () => {
    // Four states and four words. "Asked for and not working" is the one a switch must never
    // paint the same as "nobody asked", and nothing but this decides it.
    const words: [unknown, string][] = [
      [{ state: "off" }, "off"],
      [{ state: "starting", detail: "the speech model is being downloaded" }, "starting"],
      [{ state: "on", device: "Built-in" }, "listening"],
      [{ state: "failed", reason: "the microphone stopped delivering audio" }, "not listening"],
    ];
    const seen = new Set<string>();
    for (const [listening, word] of words) {
      cleanup();
      answers(machine({ listening }));
      render(<Voice />);
      expect(await screen.findByText(word)).toBeTruthy();
      seen.add(word);
    }
    expect(seen.size).toBe(4);
  });

  it("names the microphone that is open when one is", async () => {
    answers(machine({ listening: { state: "on", device: "Built-in Audio Analog Stereo" } }));

    render(<Voice />);

    await screen.findByText(/is open/i);
    expect(readable()).toContain("Built-in Audio Analog Stereo");
    // The claim that makes an open microphone acceptable, and it is checkable: the session keeps
    // nothing outside a turn.
    expect(readable()).toMatch(/nothing is recorded until you hold the push-to-talk key/i);
  });

  it("offers no switch at all on a build or a machine that cannot listen", async () => {
    answers(
      machine({
        support: {
          state: "unavailable",
          reason: "this build of Zyris was made without the audio stack, so it cannot listen",
        },
      }),
    );

    render(<Voice />);

    await screen.findByText(/made without the audio stack/i);
    expect(screen.queryByRole("button", { name: /turn on/i })).toBeNull();
  });

  it("re-reads the machine when a turn fails, and not when one merely ends", async () => {
    answers(machine({ listening: { state: "on", device: "Built-in" } }));

    render(<Voice />);
    await screen.findByText(/is open/i);
    const reads = () => invoke.mock.calls.filter(([command]) => command === "voice_state").length;
    const before = reads();

    act(() => emitVoice({ kind: "heard", text: "turn the lights off" }));
    await waitFor(() => expect(readable()).toContain("turn the lights off"));
    expect(reads()).toBe(before);

    // A microphone that goes away ends the session, and nobody clicked anything. Without this the
    // switch goes on saying "listening" over a device that is not there.
    act(() => emitVoice({ kind: "failed", reason: "the microphone stopped delivering audio" }));
    await waitFor(() => expect(reads()).toBeGreaterThan(before));
  });

  // ------------------------------------------------------------------------------------------
  // The microphone list
  // ------------------------------------------------------------------------------------------

  it("says a device list that could not be read could not be read, and never that there are none", async () => {
    answers(
      machine({
        devices: {
          state: "unreadable",
          reason: "the sound server is not running, so no microphone can be reached",
        },
      }),
    );

    render(<Voice />);

    await screen.findByText(/the sound server is not running/i);
    expect(readable()).not.toMatch(/listed no microphones/i);
    expect(screen.queryAllByRole("radio")).toHaveLength(0);
  });

  it("says there are none only when the list was read and is empty", async () => {
    answers(machine({ devices: { state: "listed", devices: [] } }));

    render(<Voice />);

    await waitFor(() => expect(readable()).toMatch(/listed no microphones/i));
  });

  it("says what each entry is, because two of them can carry the same name", async () => {
    // The list this project's own machine produces: two entries spelled identically, one of which
    // records the loudspeakers. A screen showing only names cannot be used at all.
    render(<Voice />);

    await waitFor(() => expect(screen.getAllByRole("radio").length).toBe(3));
    expect(screen.getAllByText("Built-in Audio Analog Stereo")).toHaveLength(2);
    expect(readable()).toMatch(/a microphone/i);
    expect(readable()).toMatch(/what this computer is playing/i);
  });

  it("shows which microphone is chosen, and names the one that was picked", async () => {
    answers(machine({ chosen: { kind: "device", id: "alsa_output.builtin.monitor" } }));

    render(<Voice />);

    await waitFor(() => expect(screen.getAllByRole("radio")).toHaveLength(3));
    const [followDefault, builtIn, monitor] = screen.getAllByRole<HTMLInputElement>("radio");
    // A screen that showed the wrong one as chosen would have somebody recording their
    // loudspeakers and reading that they had picked the microphone.
    expect(followDefault.checked).toBe(false);
    expect(builtIn.checked).toBe(false);
    expect(monitor.checked).toBe(true);

    builtIn.click();
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("set_voice_device", {
        device: { kind: "device", id: "alsa_input.builtin" },
      }),
    );
  });

  it("will not let a button that is already working be pressed again", async () => {
    // Starting to listen loads a 141 MB model and downloading one takes minutes. A second press
    // in the meantime is a second download, and nothing below this screen refuses it.
    invoke.mockImplementation((command: string) =>
      command === "voice_state"
        ? Promise.resolve(machine({ model: { state: "absent", path: MODEL, bytes: 147951465 } }))
        : new Promise(() => {}),
    );

    render(<Voice />);
    const download = await screen.findByRole<HTMLButtonElement>("button", {
      name: /download the speech model/i,
    });
    download.click();

    await waitFor(() => expect(readable()).toMatch(/downloading/i));
    expect(download.disabled).toBe(true);
    // And every other button on the screen, because they all go through the same machine.
    for (const button of screen.getAllByRole<HTMLButtonElement>("button")) {
      expect(button.disabled).toBe(true);
    }
  });

  // ------------------------------------------------------------------------------------------
  // The model
  // ------------------------------------------------------------------------------------------

  it("says how big the download is before anybody agrees to it", async () => {
    answers(machine({ model: { state: "absent", path: MODEL, bytes: 147951465 } }));

    render(<Voice />);

    await screen.findByRole("button", { name: /download the speech model/i });
    expect(readable()).toContain("141 MB");
    expect(readable()).toContain(MODEL);
  });

  it("offers no download for something at the model's path that could not be read", async () => {
    // A download would write to the same place and fail the same way. Three answers about one
    // file: it is here, it is not here, and there is something here nobody can read.
    answers(
      machine({
        model: {
          state: "unreadable",
          path: MODEL,
          reason: "there is something at this path and it is not a file",
        },
      }),
    );

    render(<Voice />);

    await screen.findByText(/could not read/i);
    expect(screen.queryByRole("button", { name: /download/i })).toBeNull();
    expect(readable()).not.toMatch(/has not been downloaded/i);
  });

  it("says a file of the wrong size is the wrong size, with both numbers", async () => {
    answers(
      machine({ model: { state: "damaged", path: MODEL, bytes: 1048576, expected: 147951465 } }),
    );

    render(<Voice />);

    await screen.findByRole("button", { name: /download the speech model/i });
    expect(readable()).toContain("1 MB");
    expect(readable()).toContain("141 MB");
  });

  it("neither downloads nor deletes a model file somebody named themselves", async () => {
    answers(
      machine({
        model: { state: "ready", path: "/opt/models/ggml-small.bin", bytes: 487601967 },
        modelEnv: "/opt/models/ggml-small.bin",
      }),
    );

    render(<Voice />);

    await screen.findByText(/takes that file as given/i);
    expect(screen.queryByRole("button", { name: /download/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /delete/i })).toBeNull();
  });

  it("offers to delete a model it downloaded, and says what that costs", async () => {
    render(<Voice />);

    await screen.findByRole("button", { name: /delete it/i });
    expect(readable()).toMatch(/deleting the model turns listening off/i);
  });

  // ------------------------------------------------------------------------------------------
  // The wake word
  // ------------------------------------------------------------------------------------------

  it("says nothing reads the wake word, in every state it can be in", async () => {
    for (const state of [
      { state: "nothing" },
      { state: "partial", recorded: 2 },
      { state: "complete", recorded: 5 },
      { state: "unreadable", reason: "wake-word.json could not be read" },
    ]) {
      cleanup();
      answers(
        machine({
          wake: {
            state,
            dir: TAKES,
            wanted: 5,
            seconds: 5,
            note: "Nothing listens for it yet, and saving it does not make Zyris respond to it.",
          },
        }),
      );
      render(<Voice />);
      await waitFor(() =>
        expect(readable()).toMatch(/nothing listens for it yet/i),
        // The claim that must not drift. A screen that said it only in one of the four states
        // would say it exactly where nobody has recorded anything yet.
      );
    }
  });

  it("says takes that could not be read could not be read, and never that there are none", async () => {
    answers(
      machine({
        wake: {
          state: { state: "unreadable", reason: "wake-word.json could not be read: invalid JSON" },
          dir: TAKES,
          wanted: 5,
          seconds: 5,
          note: "Nothing listens for it yet.",
        },
      }),
    );

    render(<Voice />);

    await screen.findByText(/invalid JSON/);
    // Somebody told they have recorded nothing records five more over the top of what is there.
    expect(readable()).not.toMatch(/nothing has been recorded/i);
    expect(readable()).not.toMatch(/of 5 takes recorded/i);
  });

  it("counts the takes that are there, and stops offering to record once they all are", async () => {
    answers(
      machine({
        wake: {
          state: { state: "partial", recorded: 2 },
          dir: TAKES,
          wanted: 5,
          seconds: 5,
          note: "Nothing listens for it yet.",
        },
      }),
    );
    render(<Voice />);
    await screen.findByText(/2 of 5 takes recorded/i);
    expect(screen.queryByRole("button", { name: /record a take/i })).toBeTruthy();

    cleanup();
    answers(
      machine({
        wake: {
          state: { state: "complete", recorded: 5 },
          dir: TAKES,
          wanted: 5,
          seconds: 5,
          note: "Nothing listens for it yet.",
        },
      }),
    );
    render(<Voice />);
    await screen.findByText(/all 5 takes are recorded/i);
    // Not a disabled button: there is nothing to press, and `Store::add` refuses a sixth anyway.
    expect(screen.queryByRole("button", { name: /record a take/i })).toBeNull();
    expect(screen.queryByRole("button", { name: /clear them/i })).toBeTruthy();
  });

  it("offers no recording on a machine that cannot listen", async () => {
    answers(
      machine({
        support: { state: "unavailable", reason: "this computer has no microphone" },
      }),
    );

    render(<Voice />);

    await screen.findByText(/this computer has no microphone/i);
    expect(screen.queryByRole("button", { name: /record a take/i })).toBeNull();
  });
});
