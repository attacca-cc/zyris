import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { subscribeVoice, type VoiceEvent } from "./state";

// ---------------------------------------------------------------------------------------------
// What the Rust side answers with
// ---------------------------------------------------------------------------------------------

// `zyris_voice::VoiceSupport`, internally tagged on `state`. Two answers: this machine and this
// build can listen, or they cannot and here is why.
//
// **Not the same question as "is anything listening".** A machine that can and is not has to read
// differently from one that never could, which is why `listening` below is its own answer.
type Support = { state: "ready" } | { state: "unavailable"; reason: string };

// `zyris_voice::view::ListeningState`. Four answers: nobody asked, getting there, open, and
// **asked for and not working** — the last is the one a switch must not hide.
type Listening =
  | { state: "off" }
  | { state: "starting"; detail: string }
  | { state: "on"; device: string }
  | { state: "failed"; reason: string };

// `zyris_voice::view::Direction`. Not a filter on the Rust side and not one here: on many
// computers two of the entries are loudspeakers whose monitor can be captured, and two entries
// can carry the same name, so this is the only thing that tells them apart.
type Direction = "input" | "output" | "duplex" | "unknown";

type InputDevice = {
  id: string;
  name: string;
  isDefault: boolean;
  direction: Direction;
};

// `zyris_voice::view::DeviceList`. **Three answers and only one of them is a list.** `listed`
// with nothing in it is a computer with no microphone; `unreadable` is a sound system that would
// not answer. Rendering the second as the first is the confident false negative this project has
// shipped once per screen that guessed.
type DeviceList =
  | { state: "listed"; devices: InputDevice[] }
  | { state: "unreadable"; reason: string }
  | { state: "notHere"; reason: string };

// `zyris_voice::view::Choice`, tagged on `kind`. `default` follows the system default when it
// changes; a named device does not.
type Choice = { kind: "default" } | { kind: "device"; id: string };

// `zyris_voice::view::ModelView`. `absent` carries how big the download is, so the screen can say
// what it is about to ask for before anybody presses anything. `unreadable` is separate from
// `absent` because a download does not fix it.
type ModelView =
  | { state: "ready"; path: string; bytes: number }
  | { state: "absent"; path: string; bytes: number }
  | { state: "damaged"; path: string; bytes: number; expected: number }
  | { state: "unreadable"; path: string; reason: string }
  | { state: "nowhere"; reason: string }
  | { state: "notHere"; reason: string };

// `zyris_voice::view::WakeState`. `unreadable` again kept apart from `nothing`.
type WakeState =
  | { state: "nothing" }
  | { state: "partial"; recorded: number }
  | { state: "complete"; recorded: number }
  | { state: "unreadable"; reason: string }
  | { state: "notHere"; reason: string };

type WakeView = {
  state: WakeState;
  dir: string | null;
  wanted: number;
  seconds: number;
  // `zyris_voice::wake::NOTHING_READS_THESE`, carried rather than written again here. That
  // constant has a test on each of its claims; a second copy of the sentence in TypeScript would
  // have none, and this is the claim that must not drift.
  note: string;
};

// `crate::hotkey::HotkeySupport`, internally tagged on `state`. **Three answers, and this screen
// must not flatten them**: a key that works, a key the desktop will only let the *person* bind,
// and a desktop where no application can register one at all.
// `zyris_voice::view::SpeakingState`. Kept apart from `Listening` because the two halves fail
// apart: a machine can hear perfectly and answer never, and one screen showing only the first
// would leave somebody wondering why it is silent.
type Speaking =
  | { state: "notHere"; reason: string }
  | { state: "noSession"; settings: string }
  | { state: "session"; id: string };

type HotkeySupport =
  | { state: "working"; trigger: string; releaseConfirmed: boolean }
  | { state: "needsAKeyBound"; shortcutId: string; desktop: string; line: string | null; how: string }
  | { state: "unavailable"; reason: string };

// `bridge::VoiceScreen`. One answer rather than two commands, because the two halves have to
// agree: "nothing is listening" read against a hotkey answer from a different moment is exactly
// what somebody would work out their next move from.
export type VoiceScreen = {
  voice: {
    support: Support;
    listening: Listening;
    devices: DeviceList;
    chosen: Choice;
    model: ModelView;
    modelEnv: string | null;
    wake: WakeView;
    speaking: Speaking;
  };
  hotkey: HotkeySupport;
};

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

// A rejected `invoke` carries whatever the command returned as its error. These commands return
// strings, so anything else means the bridge itself broke and is not worth showing verbatim. The
// same three lines as in Mcp.tsx and Settings.tsx, for the same reason.
function asMessage(error: unknown, fallback: string): string {
  return typeof error === "string" ? error : fallback;
}

// Whole megabytes. The model is 147,951,465 bytes, which is 141 MB, and that is the number worth
// reading before agreeing to a download.
function megabytes(bytes: number): string {
  return `${Math.round(bytes / (1024 * 1024))} MB`;
}

// What one entry in the microphone list actually is.
//
// **This is the only thing that tells two identically named entries apart**, so it is a sentence
// and not an icon. On this project's own development machine the list is four entries, two of
// which record the loudspeakers, and both pairs are spelled the same.
function whatItIs(device: InputDevice): string {
  switch (device.direction) {
    case "input":
      return "a microphone";
    case "output":
      return "a loudspeaker — recording it captures what this computer is playing, not what you say";
    case "duplex":
      return "a loudspeaker and a microphone in one — recording it may capture what this computer is playing";
    case "unknown":
      return "the sound system will not say which it is without opening it";
  }
}

// The short word beside the switch. Short because `.badge` does not wrap.
function listeningBadge(listening: Listening): { label: string; className: string } {
  switch (listening.state) {
    case "on":
      return { label: "listening", className: "badge-on" };
    case "starting":
      return { label: "starting", className: "badge-off" };
    case "off":
      return { label: "off", className: "badge-off" };
    case "failed":
      return { label: "not listening", className: "badge-down" };
  }
}

// What the last thing the voice session said looks like on the screen.
//
// There is no clock in here and nothing counts. A label that rewrote itself on a timer would be
// the one thing on this screen that changed without anything having happened.
function heardLine(event: VoiceEvent): string {
  switch (event.kind) {
    case "listening":
      return "Recording. Let go of the key when you have finished.";
    case "thinking":
      return "Working out what you said.";
    case "heard":
      return `“${event.text}”`;
    case "heardNothing":
      return "Nothing was said in that turn.";
    case "failed":
      return event.reason;
    case "speaking":
      return "Reading the answer out loud.";
    case "spoke":
      return "Finished reading the answer out loud.";
    case "interrupted":
      return "Stopped reading the answer out loud, because you started speaking.";
  }
}

// ---------------------------------------------------------------------------------------------
// The screen
// ---------------------------------------------------------------------------------------------

export function Voice() {
  // The answer, or `undefined` while the first read is in flight. Never an empty object for a
  // read that has not come back, and never one for a read that failed: see `problem`.
  const [screen, setScreen] = useState<VoiceScreen | undefined>(undefined);
  // A read that did not come back at all, kept apart from `screen` for the reason Mcp.tsx keeps
  // `problem` apart from `list`. A failed read is not an empty machine.
  const [problem, setProblem] = useState<string | null>(null);
  // Which button is in flight. Loading the speech model takes seconds and downloading it takes
  // minutes, so a button that looked instant would be the screen lying about what it is doing.
  const [busy, setBusy] = useState<string | null>(null);
  // Why the last thing pressed would not happen, beside the section it was pressed in.
  const [refused, setRefused] = useState<Record<string, string>>({});
  // The last thing the voice session said. Not folded into the core reducer: a microphone is not
  // something the node did about its connection to Attacca, and nothing outside this screen
  // needs it.
  const [last, setLast] = useState<VoiceEvent | null>(null);

  useEffect(() => {
    // The same guard the other screens use: StrictMode runs this effect twice, and both the read
    // and the subscription can land after the cleanup.
    let cancelled = false;

    function read() {
      void invoke<VoiceScreen>("voice_state")
        .then((answer) => {
          if (cancelled) return;
          setScreen(answer);
          // An answer is an answer: a message from a read that failed earlier has stopped being
          // true, and leaving it above a fresh one reads as a broken screen.
          setProblem(null);
        })
        .catch((error: unknown) => {
          if (cancelled) return;
          // Deliberately not clearing `screen`. Whatever is on it is still the last thing this
          // machine said, and blanking it would claim there is nothing here.
          setProblem(asMessage(error, "Could not read what this computer can hear with."));
        });
    }

    read();

    let unlisten: (() => void) | undefined;
    void subscribeVoice((event) => {
      if (cancelled) return;
      setLast(event);
      // **Re-read on a failure, and only on a failure.** A microphone that goes away ends the
      // session, and without this the switch would go on saying "listening" over a device that
      // is not there — the defect the MCP screen's re-read exists to prevent, one layer down.
      // The other three events change nothing a command would answer differently.
      if (event.kind === "failed") read();
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Every button goes through this: it names itself, clears its own last refusal, and puts back
  // whatever the command answered — which is the machine's state afterwards, not the request.
  function act(name: string, command: string, args?: Record<string, unknown>) {
    setBusy(name);
    setRefused((all) => {
      const { [name]: _gone, ...rest } = all;
      return rest;
    });
    void invoke<VoiceScreen>(command, args)
      .then((answer) => setScreen(answer))
      .catch((error: unknown) => {
        setRefused((all) => ({ ...all, [name]: asMessage(error, "That would not happen.") }));
        // Ask the machine again rather than leaving the screen showing what was asked for.
        return invoke<VoiceScreen>("voice_state")
          .then(setScreen)
          .catch(() => {
            // Nothing to add: the message above is already the honest one.
          });
      })
      .finally(() => setBusy(null));
  }

  if (screen === undefined) {
    return (
      <main className="screen screen-wide">
        <h1>Voice</h1>
        {problem ? (
          <p className="problem">{problem}</p>
        ) : (
          <p className="muted">Reading what this computer can hear with.</p>
        )}
      </main>
    );
  }

  const { voice, hotkey } = screen;
  const canHear = voice.support.state === "ready";
  // **No switch where there is no key.** Turning listening on would open a microphone that
  // nothing could ever start a turn on: the push-to-talk key is the only way in, and on a desktop
  // with no GlobalShortcuts interface there is no key to press. A control that cannot work is
  // worse than an absent one — the rule `announce.rs` states about `input` and `screen_capture`.
  // `needsAKeyBound` is **not** this case: the key may well already be bound, and Zyris cannot
  // tell either way.
  const aKeyCouldWork = hotkey.state !== "unavailable";
  const badge = listeningBadge(voice.listening);
  const listeningOn = voice.listening.state === "on";

  return (
    <main className="screen screen-wide">
      <h1>Voice</h1>
      <p className="lead">
        Speech into this computer. What you say is turned into text here, on this computer's own
        processor — the audio is not sent anywhere.
      </p>

      {problem && <p className="problem">{problem}</p>}

      <section>
        <h2>Listening</h2>

        {!canHear ? (
          // The reason, and no switch. This is the build with no audio stack in it, or a machine
          // whose sound system has no microphone to offer.
          <p className="problem">{(voice.support as { reason: string }).reason}</p>
        ) : (
          <>
            <div className="switch-row">
              <span className="switch-state">
                <span className={`badge ${badge.className}`}>{badge.label}</span>
              </span>
              {aKeyCouldWork && (
                <button
                  type="button"
                  className="button"
                  disabled={busy !== null}
                  onClick={() =>
                    act("listening", "set_voice_listening", { listening: !listeningOn })
                  }
                >
                  {listeningOn
                    ? busy === "listening"
                      ? "Turning off"
                      : "Turn off"
                    : busy === "listening"
                      ? "Turning on"
                      : "Turn on"}
                </button>
              )}
            </div>

            {voice.listening.state === "off" && (
              <p className="note muted">
                Nothing is listening. Zyris does not open a microphone until you turn this on.
                Your answer is kept on this computer, so it starts listening again the next time
                it runs.
              </p>
            )}
            {voice.listening.state === "starting" && (
              <p className="note muted">{voice.listening.detail}</p>
            )}
            {voice.listening.state === "on" && (
              <p className="note muted">
                The microphone <span className="mono">{voice.listening.device}</span> is open.
                Nothing is recorded until you hold the push-to-talk key.
              </p>
            )}
            {voice.listening.state === "failed" && (
              <p className="note problem">{voice.listening.reason}</p>
            )}
            {!aKeyCouldWork && (
              // The switch is gone and the reason has to be here rather than only in the section
              // below, or its absence reads as a screen that failed to draw.
              <p className="note problem">
                There is no push-to-talk key on this desktop, so a microphone opened here could
                never be asked to record anything. See below.
              </p>
            )}
            {refused.listening && <p className="note problem">{refused.listening}</p>}

            {/* No label in front of it, and no clock behind it. This says what the session last
                said and changes only when the session says something else. */}
            {last && <p className="note">{heardLine(last)}</p>}

            <p className="muted note">
              What you said is shown here and nowhere else: nothing is sent to an agent yet, and
              no recording is kept once it has been turned into text. A run started with{" "}
              <span className="mono">--headless</span> never listens — it has no window and no key
              for anybody to hold.
            </p>
          </>
        )}
      </section>

      <section>
        <h2>Reading answers aloud</h2>

        {voice.speaking.state === "notHere" && <p>{voice.speaking.reason}</p>}

        {voice.speaking.state === "noSession" && (
          <>
            <p>
              Nothing is read aloud. This computer hears you, writes down what you said and hands
              it on &mdash; it just has no Attacca session to answer from.
            </p>
            <p className="note">
              There is no control for that here yet. Put a session id in{" "}
              <span className="mono">{voice.speaking.settings}</span> under{" "}
              <span className="mono">session</span> and restart Zyris.
            </p>
          </>
        )}

        {voice.speaking.state === "session" && (
          <>
            <p>
              Answers from session <span className="mono">{voice.speaking.id}</span> are read aloud
              as they are written.
            </p>
            <p className="note">
              Speaking runs behind writing, so there are pauses between sentences. Pressing the
              push-to-talk key stops the speaking: your side of the conversation records where it
              was cut off, and the agent&rsquo;s own record of the answer stays whole.
            </p>
          </>
        )}
      </section>

      <section>
        <h2>The push-to-talk key</h2>

        {hotkey.state === "working" && (
          <>
            <p>
              Hold <span className="mono">{hotkey.trigger}</span> to talk, from any window.
            </p>
            {/* The caveat belongs to the backend, not to whether a key is bound. A screen that
                switched on the state alone would drop it the day a portal started reporting a
                trigger — which is exactly when it would start mattering. */}
            {!hotkey.releaseConfirmed && (
              <p className="muted note">
                It has not been confirmed that letting go of the key gets through to Zyris on this
                kind of desktop. If a turn does not end when you let go, Zyris ends it on its own
                and throws the recording away rather than sending half a sentence on.
              </p>
            )}
          </>
        )}

        {hotkey.state === "needsAKeyBound" && (
          <>
            <p>{hotkey.how}</p>
            {hotkey.line !== null ? (
              <>
                <pre className="snippet">{hotkey.line}</pre>
                <p className="muted note">
                  That is the line for {hotkey.desktop}. The shortcut it points at is called{" "}
                  <span className="mono">{hotkey.shortcutId}</span>.
                </p>
              </>
            ) : (
              <p className="muted note">
                Zyris does not know how {hotkey.desktop} spells that line, so it does not show
                one: a wrong line pasted into a configuration file costs an evening. Bind a key to
                the global shortcut named <span className="mono">{hotkey.shortcutId}</span> in
                your desktop's own keyboard settings.
              </p>
            )}
            <p className="muted note">
              Zyris cannot tell whether you have bound a key — your desktop does not say. It also
              has not been confirmed here that letting go of the key gets through to Zyris on this
              kind of desktop. If a turn does not end when you let go, Zyris ends it on its own and
              throws the recording away rather than sending half a sentence on.
            </p>
          </>
        )}

        {hotkey.state === "unavailable" && (
          <p className="problem">
            {hotkey.reason} Nothing on this screen can start a turn until that changes.
          </p>
        )}
      </section>

      <section>
        <h2>Microphone</h2>

        {voice.devices.state === "notHere" && <p className="problem">{voice.devices.reason}</p>}

        {voice.devices.state === "unreadable" && (
          // Never "no microphones" on a failed read. A sound server that is not running answers
          // nothing at all, which is a different thing from a computer that has no microphone —
          // and telling somebody the second when it is the first sends them shopping.
          <p className="problem">
            The list of microphones could not be read, so there is nothing to choose from here.{" "}
            {voice.devices.reason}
          </p>
        )}

        {voice.devices.state === "listed" && voice.devices.devices.length === 0 && (
          <p className="muted">
            The sound system answered and listed no microphones on this computer.
          </p>
        )}

        {voice.devices.state === "listed" && voice.devices.devices.length > 0 && (
          <>
            <ul className="caps choices">
              <li>
                <label>
                  <input
                    type="radio"
                    name="microphone"
                    checked={voice.chosen.kind === "default"}
                    disabled={busy !== null}
                    onChange={() =>
                      act("device", "set_voice_device", { device: { kind: "default" } })
                    }
                  />{" "}
                  Whichever this computer calls the default
                </label>
                <p className="note muted">
                  Zyris follows the default when you change it. A named device below does not
                  move.
                </p>
              </li>
              {voice.devices.devices.map((device) => (
                <li key={device.id}>
                  <label>
                    <input
                      type="radio"
                      name="microphone"
                      checked={
                        voice.chosen.kind === "device" && voice.chosen.id === device.id
                      }
                      disabled={busy !== null}
                      onChange={() =>
                        act("device", "set_voice_device", {
                          device: { kind: "device", id: device.id },
                        })
                      }
                    />{" "}
                    {device.name}
                  </label>
                  <p className="note muted">
                    {device.isDefault ? "This computer's default, and " : ""}
                    {whatItIs(device)}.
                  </p>
                </li>
              ))}
            </ul>
            <p className="muted note">
              Two entries can carry exactly the same name — on many computers a loudspeaker's
              monitor is listed beside the microphone built into the same chip. The line under
              each one is what tells them apart. Nothing is filtered out of this list: on some
              sound systems the device that always works is the one that will not say what it is.
            </p>
          </>
        )}
        {refused.device && <p className="note problem">{refused.device}</p>}
      </section>

      <section>
        <h2>Speech model</h2>

        {voice.model.state === "notHere" && <p className="problem">{voice.model.reason}</p>}
        {voice.model.state === "nowhere" && (
          <p className="problem">
            {voice.model.reason} Zyris will use a model file you point it at yourself: set{" "}
            <span className="mono">ZYRIS_WHISPER_MODEL</span> to its path.
          </p>
        )}
        {voice.model.state === "unreadable" && (
          // Not "it has not been downloaded". A download would write to the same place and fail
          // the same way, and offering one here sends a person round a loop.
          <p className="problem">
            There is something at <span className="mono">{voice.model.path}</span> that Zyris
            could not read, so the speech model could not be loaded. {voice.model.reason}
          </p>
        )}

        {voice.model.state === "ready" && (
          <p>
            The speech model is on this computer, at{" "}
            <span className="mono">{voice.model.path}</span> ({megabytes(voice.model.bytes)}).
          </p>
        )}
        {voice.model.state === "absent" && (
          <p>
            The speech model has not been downloaded yet. It is about{" "}
            {megabytes(voice.model.bytes)}, it is downloaded once, and it is kept at{" "}
            <span className="mono">{voice.model.path}</span>.
          </p>
        )}
        {voice.model.state === "damaged" && (
          <p className="problem">
            The file at <span className="mono">{voice.model.path}</span> is{" "}
            {megabytes(voice.model.bytes)} and the speech model is{" "}
            {megabytes(voice.model.expected)}, so it is not the model and cannot be loaded.
            Downloading it again replaces it.
          </p>
        )}

        {voice.modelEnv !== null ? (
          // No button at all for a file somebody named themselves. Downloading would fetch into
          // the cache that this setting overrides, and deleting would throw away a file that is
          // not Zyris's to throw away.
          <p className="muted note">
            The model in use was named by <span className="mono">ZYRIS_WHISPER_MODEL</span> (
            <span className="mono">{voice.modelEnv}</span>). Zyris takes that file as given: it
            does not check its size, will not replace it, and will not delete it. Remove that
            setting to go back to the model Zyris downloads.
          </p>
        ) : (
          <div className="switch-row">
            {(voice.model.state === "absent" || voice.model.state === "damaged") && (
              <button
                type="button"
                className="button"
                disabled={busy !== null}
                onClick={() => act("model", "fetch_speech_model")}
              >
                {busy === "model" ? "Downloading" : "Download the speech model"}
              </button>
            )}
            {voice.model.state === "ready" && (
              <button
                type="button"
                className="button button-quiet"
                disabled={busy !== null}
                onClick={() => act("model", "forget_speech_model")}
              >
                {busy === "model" ? "Deleting" : "Delete it"}
              </button>
            )}
          </div>
        )}

        {busy === "model" && (
          <p className="note muted">
            This takes a few minutes on a slow connection, and Zyris keeps nothing until the
            whole file has arrived and been checked.
          </p>
        )}
        {refused.model && <p className="note problem">{refused.model}</p>}

        <p className="muted note">
          Transcription runs on this computer and needs a processor with AVX2, which Intel and AMD
          have shipped since 2013. Deleting the model turns listening off, because there would be
          nothing left to turn what you say into text.
        </p>
      </section>

      <section>
        <h2>Wake word</h2>

        {voice.wake.state.state === "notHere" ? (
          <p className="problem">{voice.wake.state.reason}</p>
        ) : (
          <>
            {/* The sentence that must not drift, carried from `wake::NOTHING_READS_THESE` rather
                than written here. First, before anything that could read as a feature. */}
            <p>{voice.wake.note}</p>

            {voice.wake.state.state === "unreadable" && (
              // Not "nothing recorded". Something is there and it could not be read, and a person
              // told they have recorded nothing would record five more takes over the top.
              <p className="problem">
                Zyris could not read what has been recorded, so it cannot say how much of the wake
                word is there. {voice.wake.state.reason}
              </p>
            )}
            {voice.wake.state.state === "nothing" && (
              <p className="muted">
                Nothing has been recorded. {voice.wake.wanted} takes are wanted.
              </p>
            )}
            {voice.wake.state.state === "partial" && (
              <p className="muted">
                {voice.wake.state.recorded} of {voice.wake.wanted} takes recorded.
              </p>
            )}
            {voice.wake.state.state === "complete" && (
              <p className="muted">
                All {voice.wake.state.recorded} takes are recorded. To record the wake word again,
                clear these first.
              </p>
            )}

            <div className="switch-row">
              {canHear && voice.wake.state.state !== "complete" && (
                <button
                  type="button"
                  className="button button-quiet"
                  disabled={busy !== null}
                  onClick={() => act("wake", "record_wake_take")}
                >
                  {busy === "wake" ? "Recording — say it now" : "Record a take"}
                </button>
              )}
              {(voice.wake.state.state === "partial" ||
                voice.wake.state.state === "complete") && (
                <button
                  type="button"
                  className="button button-quiet"
                  disabled={busy !== null}
                  onClick={() => act("wakeClear", "clear_wake_word")}
                >
                  {busy === "wakeClear" ? "Clearing" : "Clear them"}
                </button>
              )}
            </div>
            {refused.wake && <p className="note problem">{refused.wake}</p>}
            {refused.wakeClear && <p className="note problem">{refused.wakeClear}</p>}

            {canHear && (
              <p className="muted note">
                Recording starts as soon as you press the button and stops when you stop speaking,
                or after {voice.wake.seconds} seconds. It uses the microphone chosen above,
                whether or not listening is on.
              </p>
            )}
            {voice.wake.dir !== null && (
              <p className="muted note">
                The recordings are kept at <span className="mono">{voice.wake.dir}</span>.
              </p>
            )}
          </>
        )}
      </section>
    </main>
  );
}
