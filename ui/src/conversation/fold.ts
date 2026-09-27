import type { Trace } from "../state";

// The conversation as it happens, folded out of the voice trace.
//
// Written as a reducer over the trace rather than as state the Rust side keeps, because there is
// no second copy to go stale: the window shows what it was told, in the order it was told, and a
// gap in the stream shows as a gap rather than as a wrong total.
//
// Three things are worth knowing before reading it.
//
// **What you say is not transcribed live.** Whisper is not a streaming recogniser. While the key
// is down it re-reads the whole recording every second and a half (`hearing`), and each guess
// replaces the last; the text is final when `transcribed` arrives.
//
// **Synthesis runs behind the writing.** The agent finishes an answer well before the speaker
// finishes reading it, so it is normal for every sentence to be written while only the first is
// sounding. That is what the colours in `colors.ts` show.
//
// **The playback position is samples written to the device, not a clock.** It is per answer
// here: a speaker opened again counts from zero, and a position carried over from an older answer
// would make a new one look already heard.

// A sentence the splitter cut out of the answer, and how far it has got.
export type Sentence = {
  text: string;
  // Seconds of audio, once the voice has made it.
  seconds: number | null;
  // Where it sits in the speaker's stream, once queued.
  at: number | null;
  samples: number | null;
  // Refused before it was played — nobody will ever hear it.
  dropped: boolean;
};

export type YouTurn = {
  who: "you";
  // recording → transcribing → said (text back, or typed) → sent; or lost at any point.
  state: "recording" | "transcribing" | "said" | "sent" | "lost";
  text: string;
  // Whisper's latest guess while the key is down.
  sofar: string;
  // Why a lost turn was lost.
  detail: string;
};

export type AgentTurn = {
  who: "agent";
  // What the agent wrote, reasoning left out.
  text: string;
  sentences: Sentence[];
  // Whether this answer is being read aloud. When not, no sentence follows and it is all white.
  aloud: boolean;
  // Still being written.
  writing: boolean;
  // Nothing more will be read: speech ended, was cut off, or something came after it.
  settled: boolean;
  interrupted: boolean;
  // Samples of this answer's stream written to the device, or -1 before any.
  played: number;
  // What the agent last said it was doing, and how many tools it has called, for the line that
  // shows it is working rather than stuck.
  activity: string | null;
  tools: number;
  // The agent stopped writing to work (a tool, reasoning): its next words start a paragraph.
  breakNext: boolean;
};

export type Turn = YouTurn | AgentTurn | { who: "problem"; reason: string };

// A session's earlier messages, as turns that are over: sent, and answered in plain white.
export type HistoryLine = { who: "you" | "agent"; text: string };
export function fromHistory(lines: HistoryLine[]): Turn[] {
  return lines.map((line) =>
    line.who === "you"
      ? { ...newYou("sent", line.text) }
      : { ...newAgent(false), text: line.text, writing: false, settled: true },
  );
}

// A trace step, or something this window did itself: a typed message goes on screen before the
// bridge says it was sent, so the person sees it at once.
export type Action =
  | Trace
  | { step: "typed"; text: string }
  | { step: "typedFailed"; text: string; reason: string };

function newAgent(aloud: boolean): AgentTurn {
  return {
    who: "agent",
    text: "",
    sentences: [],
    aloud,
    writing: true,
    settled: false,
    interrupted: false,
    played: -1,
    activity: null,
    tools: 0,
    breakNext: false,
  };
}

function newYou(state: YouTurn["state"], text = ""): YouTurn {
  return { who: "you", state, text, sofar: "", detail: "" };
}

// Everything already on screen stops expecting more speech once a new turn begins.
function settleAll(turns: Turn[]): Turn[] {
  return turns.map((turn) =>
    turn.who === "agent" && !turn.settled ? { ...turn, settled: true, writing: false } : turn,
  );
}

function lastIndex(turns: Turn[], match: (turn: Turn) => boolean): number {
  for (let i = turns.length - 1; i >= 0; i -= 1) if (match(turns[i])) return i;
  return -1;
}

export function fold(turns: Turn[], action: Action): Turn[] {
  const lastTurn = turns[turns.length - 1];
  const you = lastTurn?.who === "you" ? lastTurn : null;
  // A turn of yours still on its way to becoming text.
  const open = you && (you.state === "recording" || you.state === "transcribing") ? you : null;
  // **The latest answer, wherever it is.** Something typed while an answer is still streaming
  // sits after it, and the rest of that answer — text, sentences, playback — still belongs to it.
  // A new answer is always announced by `answering`, so nothing is misfiled.
  const agentAt = lastIndex(turns, (t) => t.who === "agent");
  const agent = agentAt >= 0 ? (turns[agentAt] as AgentTurn) : null;

  const replaceAt = (index: number, turn: Turn) => turns.map((t, i) => (i === index ? turn : t));
  const replaceLast = (turn: Turn) => replaceAt(turns.length - 1, turn);
  const updateAgent = (change: (turn: AgentTurn) => AgentTurn) =>
    agent ? replaceAt(agentAt, change(agent)) : turns;
  // The newest sentence matching `text`, searched from the end: a repeated sentence in one answer
  // is ordinary and the one being worked on is always the latest of them.
  const withSentence = (text: string, change: (sentence: Sentence) => Sentence): Turn[] => {
    if (!agent) return turns;
    const index = agent.sentences.map((s) => s.text).lastIndexOf(text);
    if (index < 0) return turns;
    return updateAgent((a) => ({ ...a, sentences: a.sentences.map((s, i) => (i === index ? change(s) : s)) }));
  };
  // Progress belongs to the answer being written. Anything that is not its words means the next
  // words it writes start a new paragraph.
  const working = (change: (turn: AgentTurn) => AgentTurn): Turn[] =>
    agent && agent.writing ? updateAgent((a) => ({ ...change(a), breakNext: a.text.trim() !== "" })) : turns;
  const lose = (index: number, reason: string): Turn[] =>
    index < 0
      ? [...turns, { who: "problem", reason }]
      : replaceAt(index, { ...(turns[index] as YouTurn), state: "lost", detail: reason });

  switch (action.step) {
    case "recording":
      if (action.started) return [...turns, newYou("recording")];
      return open?.state === "recording" ? replaceLast({ ...open, state: "transcribing" }) : turns;
    case "transcribing":
      return open ? replaceLast({ ...open, state: "transcribing" }) : turns;
    case "hearing":
      // Replaced whole, never appended to: whisper re-reads the recording from the start.
      return open ? replaceLast({ ...open, sofar: action.text }) : turns;
    case "recorded":
      if (!open || action.kept) return turns;
      return replaceLast({ ...open, state: "lost", detail: "Too little speech to send." });
    case "transcribed":
      if (!open) return turns;
      return action.text === ""
        ? replaceLast({ ...open, state: "lost", detail: "No speech was found." })
        : replaceLast({ ...open, state: "said", text: action.text });
    case "typed":
      return action.text.trim() === "" ? turns : [...turns, newYou("said", action.text)];
    case "sent": {
      const index = lastIndex(turns, (t) => t.who === "you" && t.state === "said" && t.text === action.text);
      if (index >= 0) return replaceAt(index, { ...(turns[index] as YouTurn), state: "sent" });
      return [...turns, newYou("sent", action.text)];
    }
    case "sendFailed":
      return lose(lastIndex(turns, (t) => t.who === "you" && t.state === "said"), action.reason);
    case "typedFailed":
      return lose(
        lastIndex(turns, (t) => t.who === "you" && t.state === "said" && t.text === action.text),
        action.reason,
      );
    case "failed":
      // A turn of yours that never got as far as its text is what failed; anything else is said
      // as a line of its own, rather than left for the screen to go on claiming it is working.
      return open
        ? replaceLast({ ...open, state: "lost", detail: action.reason })
        : [...turns, { who: "problem", reason: action.reason }];
    case "answering":
      // A new answer is a new turn even with nothing said here in between — one typed on another
      // device, say. An empty one already open (a fragment raced ahead) is reused.
      if (agent && agentAt === turns.length - 1 && agent.text === "" && agent.sentences.length === 0) {
        return replaceLast(newAgent(action.aloud));
      }
      return [...settleAll(turns), newAgent(action.aloud)];
    case "delta":
      // Reasoning is the agent working, not its answer — but words after it start afresh.
      if (action.kind !== "Assistant") return working((a) => a);
      return agent && !agent.settled
        ? updateAgent((a) => ({
            ...a,
            text: a.breakNext && a.text.trim() !== "" ? `${a.text.trimEnd()}\n\n${action.text.trimStart()}` : a.text + action.text,
            breakNext: false,
          }))
        : [...settleAll(turns), { ...newAgent(false), text: action.text }];
    case "working":
      return working((a) => ({ ...a, activity: action.title }));
    case "tool":
      return working((a) => ({ ...a, tools: a.tools + 1 }));
    case "answered":
      return updateAgent((a) => ({ ...a, writing: false }));
    case "fragment": {
      const sentence: Sentence = { text: action.text, seconds: null, at: null, samples: null, dropped: false };
      return agent && !agent.settled
        ? updateAgent((a) => ({ ...a, sentences: [...a.sentences, sentence] }))
        : [...settleAll(turns), { ...newAgent(true), sentences: [sentence] }];
    }
    case "synthesised":
      return withSentence(action.text, (s) => ({ ...s, seconds: action.seconds }));
    case "queued":
      return withSentence(action.text, (s) => ({ ...s, at: action.atSample, samples: action.samples }));
    case "dropped":
      // No text on this one — the fragment was refused before it was queued. The newest sentence
      // that never reached the queue is the one it was.
      return updateAgent((a) => {
        for (let i = a.sentences.length - 1; i >= 0; i -= 1) {
          if (a.sentences[i].at === null && !a.sentences[i].dropped) {
            return { ...a, sentences: a.sentences.map((s, j) => (j === i ? { ...s, dropped: true } : s)) };
          }
        }
        return a;
      });
    // **Every answer still being read, not only the newest.** A question asked while the last
    // answer is still playing opens a new answer, and the rest of the old one goes on sounding;
    // moving only the newest left the old one's last sentences grey for good. The position is the
    // speaker's own count of samples, so a sentence queued later is never reached early.
    case "playing":
      return turns.map((t) =>
        t.who === "agent" && t.aloud && !t.interrupted ? { ...t, played: Math.max(t.played, action.atSample) } : t,
      );
    case "spoke":
      // The speaker ran dry. An answer still being written with nothing of its own queued yet is
      // not what went quiet: settling it would file its next sentence as a new answer.
      return turns.map((t) =>
        t.who === "agent" && (!t.writing || t.sentences.some((s) => s.at !== null)) ? { ...t, settled: true } : t,
      );
    case "interrupted":
      // Whichever answers still had audio waiting, and the newest if it was still going: that is
      // what the key cut off.
      return turns.map((t, i) =>
        t.who === "agent" &&
        ((i === agentAt && !t.settled) ||
          t.sentences.some((s) => !s.dropped && (s.at === null || s.at + (s.samples ?? 0) > t.played)))
          ? { ...t, interrupted: true, settled: true }
          : t,
      );
    default:
      return turns;
  }
}
