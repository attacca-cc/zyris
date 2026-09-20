import { useEffect, useRef, useState } from "react";
import { subscribeTrace, type Trace } from "./state";

// The conversation as it happens, with the speaking half shown sentence by sentence.
//
// **Not the same screen as Debug and not a prettier one.** Debug is the pipeline: every step in
// order, including the ones that are about the machine rather than the conversation. This is
// what was said, by whom, and — for the agent's side — how far each sentence has got through
// being turned into audio and then through being heard.
//
// Three things are worth knowing before reading it.
//
// **What you say is not transcribed live.** Whisper is not a streaming recogniser: it is handed
// a whole turn when the key comes up and answers once, about a second later. So a turn shows as
// recording and then as its text, and there is no half-sentence in between. Anything that
// appeared to type itself out would be a lie about where the words come from.
//
// **Synthesis runs behind the writing.** The agent finishes an answer well before the speaker
// finishes reading it, measured at 1.2 to 1.9 times real time, so it is normal for every
// sentence to be written while only the first is sounding.
//
// **The playback cursor is samples written to the device, not a clock.** It is ahead of the
// loudspeaker by whatever the device buffers — about 43 ms — and that is the whole of the
// uncertainty. A bar driven by a timer would drift and would keep filling after an
// interruption threw the queue away.

// A sentence the splitter cut out of the answer, and how far it has got.
type Sentence = {
  text: string;
  // Seconds of audio, once the voice has made it.
  seconds: number | null;
  // Where it sits in the speaker's stream, once queued. `null` until then.
  at: number | null;
  samples: number | null;
  // Finished after the key went down, so nobody will ever hear it.
  dropped: boolean;
};

type Turn =
  | { who: "you"; state: "recording" | "thinking" | "said" | "lost"; text: string; detail: string }
  | { who: "agent"; text: string; sentences: Sentence[]; interrupted: boolean };

// How far through a sentence the speaker is: null when it has not started, 1 when it is done.
function progress(sentence: Sentence, cursor: number): number | null {
  if (sentence.at === null || sentence.samples === null || sentence.samples === 0) return null;
  if (cursor <= sentence.at) return null;
  return Math.min(1, (cursor - sentence.at) / sentence.samples);
}

function say(seconds: number): string {
  return `${seconds.toFixed(1)}s`;
}

// Fold one step into the turns so far.
//
// Written as a reducer over the trace rather than as state the Rust side keeps, because there
// is no second copy to go stale: the window is showing what it was told, in the order it was
// told, and a gap in the stream shows as a gap rather than as a wrong total.
function fold(turns: Turn[], step: Trace): Turn[] {
  const last = turns[turns.length - 1];
  const agent = last?.who === "agent" ? last : null;
  const you = last?.who === "you" ? last : null;

  const replaceLast = (turn: Turn) => [...turns.slice(0, -1), turn];
  // Change the newest sentence matching `text`, searching from the end: a repeated sentence in
  // one answer is ordinary and the one being worked on is always the latest of them.
  const withSentence = (text: string, change: (sentence: Sentence) => Sentence): Turn[] => {
    if (!agent) return turns;
    const at = agent.sentences.map((s) => s.text).lastIndexOf(text);
    if (at < 0) return turns;
    const sentences = agent.sentences.map((s, i) => (i === at ? change(s) : s));
    return replaceLast({ ...agent, sentences });
  };

  switch (step.step) {
    case "recording":
      if (step.started) {
        return [...turns, { who: "you", state: "recording", text: "", detail: "" }];
      }
      return you && you.state === "recording"
        ? replaceLast({ ...you, state: "thinking" })
        : turns;
    case "recorded":
      if (!you || step.kept) return turns;
      return replaceLast({
        ...you,
        state: "lost",
        detail:
          `only ${say(step.speechSeconds)} of speech in ${say(step.seconds)} — below the ` +
          "floor, so it was not transcribed",
      });
    case "transcribed":
      if (!you) return turns;
      return step.text === ""
        ? replaceLast({ ...you, state: "lost", detail: "no speech was found in it" })
        : replaceLast({ ...you, state: "said", text: step.text });
    case "sendFailed":
      return you ? replaceLast({ ...you, state: "lost", detail: step.reason }) : turns;
    case "delta":
      // Reasoning is the agent working, not its answer. It belongs in Debug, not here.
      if (step.kind !== "Assistant") return turns;
      return agent
        ? replaceLast({ ...agent, text: agent.text + step.text })
        : [...turns, { who: "agent", text: step.text, sentences: [], interrupted: false }];
    case "fragment": {
      const sentence: Sentence = {
        text: step.text,
        seconds: null,
        at: null,
        samples: null,
        dropped: false,
      };
      return agent
        ? replaceLast({ ...agent, sentences: [...agent.sentences, sentence] })
        : [...turns, { who: "agent", text: "", sentences: [sentence], interrupted: false }];
    }
    case "synthesised":
      return withSentence(step.text, (s) => ({ ...s, seconds: step.seconds }));
    case "queued":
      return withSentence(step.text, (s) => ({
        ...s,
        at: step.atSample,
        samples: step.samples,
      }));
    case "dropped":
      // No text on this one — the fragment was refused before it was ledgered. The newest
      // sentence that never reached the queue is the one it was.
      if (!agent) return turns;
      for (let i = agent.sentences.length - 1; i >= 0; i -= 1) {
        if (agent.sentences[i].at === null) {
          const sentences = agent.sentences.map((s, j) =>
            j === i ? { ...s, dropped: true } : s,
          );
          return replaceLast({ ...agent, sentences });
        }
      }
      return turns;
    case "interrupted":
      return agent ? replaceLast({ ...agent, interrupted: true }) : turns;
    default:
      return turns;
  }
}

function Yours({ turn }: { turn: Extract<Turn, { who: "you" }> }) {
  return (
    <li className="turn turn-you">
      <span className="turn-who">You</span>
      {turn.state === "recording" && <p className="note">listening&hellip;</p>}
      {/* Whisper answers once, about a second later. There is no half-sentence to show. */}
      {turn.state === "thinking" && <p className="note">writing it down&hellip;</p>}
      {turn.state === "said" && <p>{turn.text}</p>}
      {turn.state === "lost" && <p className="note">Nothing was sent &mdash; {turn.detail}</p>}
    </li>
  );
}

function Theirs({
  turn,
  cursor,
}: {
  turn: Extract<Turn, { who: "agent" }>;
  cursor: number;
}) {
  return (
    <li className="turn turn-agent">
      <span className="turn-who">Attacca</span>
      {turn.text !== "" && <p>{turn.text}</p>}

      {turn.sentences.length > 0 && (
        <ol className="sentences">
          {turn.sentences.map((sentence, at) => {
            const played = progress(sentence, cursor);
            return (
              <li key={at} className="sentence">
                <span className="sentence-text">{sentence.text}</span>
                <span className="sentence-state">
                  {sentence.dropped
                    ? "not heard"
                    : sentence.seconds === null
                      ? "waiting for the voice"
                      : sentence.at === null
                        ? `${say(sentence.seconds)} made`
                        : played === null
                          ? `${say(sentence.seconds)} queued`
                          : played >= 1
                            ? `${say(sentence.seconds)} heard`
                            : `${Math.round(played * 100)}% of ${say(sentence.seconds)}`}
                </span>
                {/* The bar is the same fact as the words beside it, never the only one. */}
                <span className="sentence-bar">
                  <span style={{ width: `${Math.round((played ?? 0) * 100)}%` }} />
                </span>
              </li>
            );
          })}
        </ol>
      )}

      {turn.interrupted && (
        <p className="note">
          You started talking, so the rest was not read aloud. The agent&rsquo;s own record of
          the answer is whole &mdash; only the speaking was cut short.
        </p>
      )}
    </li>
  );
}

export function Conversation() {
  const [turns, setTurns] = useState<Turn[]>([]);
  const [cursor, setCursor] = useState(0);
  const bottom = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let stop: (() => void) | null = null;
    let gone = false;
    subscribeTrace((step) => {
      if (step.step === "playing") setCursor(step.atSample);
      else setTurns((turns) => fold(turns, step));
    }).then((unlisten) => {
      if (gone) unlisten();
      else stop = unlisten;
    });
    return () => {
      gone = true;
      stop?.();
    };
  }, []);

  useEffect(() => {
    bottom.current?.scrollIntoView?.({ block: "end" });
  }, [turns]);

  return (
    <div className="screen">
      <h1>Conversation</h1>

      {turns.length === 0 ? (
        <p className="note">
          Nothing yet. Hold the push-to-talk key and say something. There is no wake word:
          nothing matches the takes recorded on the Voice tab, so the key is the only way to
          start a turn.
        </p>
      ) : (
        <ol className="turns">
          {turns.map((turn, at) =>
            turn.who === "you" ? (
              <Yours key={at} turn={turn} />
            ) : (
              <Theirs key={at} turn={turn} cursor={cursor} />
            ),
          )}
        </ol>
      )}

      <p className="note">
        What you say is written down in one go when you let go of the key &mdash; whisper is not
        a streaming recogniser, so there is no half-sentence to show. The agent&rsquo;s side is
        read aloud behind the writing, so it is normal for the whole answer to be on screen
        while only the first sentence is sounding. &ldquo;Heard&rdquo; means written to the
        sound device, which is about 43&nbsp;ms ahead of the loudspeaker.
      </p>
      <div ref={bottom} />
    </div>
  );
}
