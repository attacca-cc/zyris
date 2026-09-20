import { useEffect, useRef, useState } from "react";
import { subscribeTrace, type Trace } from "./state";

// Every step the audio takes, as it happens.
//
// **This screen exists because the pipeline has six places it can stop and only one of them
// says anything.** A person who presses the key and hears nothing back cannot tell a key the
// compositor never sent from a turn the silence rule threw away from a transcript that never
// reached Attacca from an answer that arrived and was filtered down to nothing. Each of those
// is one line here.
//
// It is deliberately a log and not a diagram: the order and the timing are most of what is
// being read, and a state machine drawn on screen would lose both.

// How many steps are kept. A turn is a dozen and an answer of ten sentences is fifty, so this
// is a few minutes of conversation. Dropping the oldest is right for a window somebody opens
// and then does something: what they want is what just happened.
const KEPT = 400;

type Line = { at: number; step: Trace };

// Which half of the pipeline a step belongs to, so the two can be told apart at a glance and
// filtered. `in` is microphone to Attacca, `out` is Attacca to loudspeaker.
function half(step: Trace): "in" | "out" | "bad" {
  switch (step.step) {
    case "key":
    case "recording":
    case "recorded":
    case "hearing":
    case "transcribing":
    case "transcribed":
    case "sent":
      return "in";
    case "sendFailed":
    case "failed":
      return "bad";
    default:
      return "out";
  }
}

function seconds(value: number): string {
  return `${value.toFixed(2)}s`;
}

// One line of the log. Every branch names the step and then says the one number or string that
// makes it worth having — a line that only repeated its own name would be noise in a screen
// whose whole value is that it is readable at speed.
function describe(step: Trace): string {
  switch (step.step) {
    case "key":
      // Reaching Zyris and reaching a running session are two facts. This is the first, and
      // it is published whether or not anything is listening.
      return step.down ? "key down" : "key up";
    case "recording":
      return step.started ? "recording started" : "recording stopped";
    case "recorded":
      return step.kept
        ? `recorded ${seconds(step.seconds)}, ${seconds(step.speechSeconds)} of it speech`
        : `recorded ${seconds(step.seconds)} with only ${seconds(step.speechSeconds)} of speech ` +
          "— below the floor, so it was thrown away and whisper never saw it";
    case "transcribing":
      return `whisper is working on ${seconds(step.seconds)}`;
    case "hearing":
      return `so far (${seconds(step.seconds)}): "${step.text}"`;
    case "transcribed":
      return step.text === ""
        ? `whisper found no speech in it (${step.tookMs} ms)`
        : `whisper: "${step.text}" (${step.tookMs} ms)`;
    case "sent":
      return `sent to Attacca: "${step.text}"`;
    case "sendFailed":
      return `it did not reach Attacca: ${step.reason}`;
    case "delta":
      return `the agent wrote (${step.kind}): ${step.text}`;
    case "fragment":
      return `to be spoken: "${step.text}"`;
    case "synthesised":
      return `the voice made ${seconds(step.seconds)} of audio: "${step.text}" (${step.tookMs} ms)`;
    case "queued":
      return `queued at sample ${step.atSample}: "${step.text}"`;
    case "playing":
      return `played as far as sample ${step.atSample}`;
    case "dropped":
      return "a fragment was finished after the key went down, so nobody heard it";
    case "spoke":
      return "the speaker ran out — the room is quiet";
    case "interrupted":
      return `interrupted: ${step.heard} heard, ${step.unheard} not`;
    case "failed":
      return step.reason;
  }
}

function clock(at: number): string {
  const when = new Date(at);
  const pad = (value: number, width = 2) => String(value).padStart(width, "0");
  return (
    `${pad(when.getHours())}:${pad(when.getMinutes())}:${pad(when.getSeconds())}` +
    `.${pad(when.getMilliseconds(), 3)}`
  );
}

export function Debug() {
  const [lines, setLines] = useState<Line[]>([]);
  const [following, setFollowing] = useState(true);
  const bottom = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let stop: (() => void) | null = null;
    let gone = false;
    subscribeTrace((step) => {
      setLines((kept) => [...kept, { at: Date.now(), step }].slice(-KEPT));
    }).then((unlisten) => {
      // The screen may have been left before the subscription resolved.
      if (gone) unlisten();
      else stop = unlisten;
    });
    return () => {
      gone = true;
      stop?.();
    };
  }, []);

  useEffect(() => {
    // Optional call: jsdom does not implement `scrollIntoView`, and a screen whose whole job
    // is to be readable must not fail to render because it could not scroll.
    if (following) bottom.current?.scrollIntoView?.({ block: "end" });
  }, [lines, following]);

  return (
    <div className="screen">
      <h1>Debug</h1>
      <p className="note">
        Every step the audio takes, as it happens. Nothing here is kept: this is what has
        happened since the screen was opened, newest at the bottom.
      </p>

      <p>
        <label>
          <input
            type="checkbox"
            checked={following}
            onChange={(event) => setFollowing(event.target.checked)}
          />{" "}
          Follow
        </label>{" "}
        <button type="button" onClick={() => setLines([])} disabled={lines.length === 0}>
          Clear
        </button>
      </p>

      {lines.length === 0 ? (
        <p className="note">
          Nothing yet. Hold the push-to-talk key and say something.
        </p>
      ) : null}
      {lines.length === 0 ? (
        <p className="note">
          No <span className="mono">key down</span> at all means the key is not reaching Zyris —
          another program may hold the combination, and on Wayland the compositor has to be told
          to send it. A <span className="mono">key down</span> with no{" "}
          <span className="mono">recording started</span> after it means the key arrived and
          nothing was listening: the Voice tab says why, and the usual reason is that the speech
          model has not been downloaded.
        </p>
      ) : (
        <ol className="trace">
          {lines.map((line, at) => (
            <li key={at} className={`trace-${half(line.step)}`}>
              <span className="mono trace-at">{clock(line.at)}</span>{" "}
              <span>{describe(line.step)}</span>
            </li>
          ))}
        </ol>
      )}
      <div ref={bottom} />
    </div>
  );
}
