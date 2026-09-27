import type { AgentTurn, Sentence } from "./fold";

// How far each part of an answer has got, as the thread colours it.
//
// | tone       | colour     | meaning                                                      |
// |------------|------------|--------------------------------------------------------------|
// | `pending`  | dark red   | written, no audio made for it yet                             |
// | `voiced`   | grey       | audio made, not played yet                                    |
// | `spoken`   | white      | being read, or read                                           |
// | `filtered` | red        | will never be read: dropped, or taken out by the filter       |
// | `plain`    | white      | the answer is not being read aloud at all                     |
export type Tone = "pending" | "voiced" | "spoken" | "filtered" | "plain";

export type Span = { text: string; tone: Tone };

// The streamed text reduced to its letters and digits, lower-cased, with where each one came
// from. **What the filter does to a sentence is almost all punctuation and markup** — code fences,
// emphasis, spacing — so matching on what is left finds a sentence in the text it came from
// without knowing the filter's rules.
function normalise(text: string): { norm: string; from: number[] } {
  let norm = "";
  const from: number[] = [];
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (/[\p{L}\p{N}]/u.test(ch)) {
      norm += ch.toLowerCase();
      from.push(i);
    }
  }
  return { norm, from };
}

// How many letters of a sentence's start and end are enough to find it when the middle was
// changed — an aside in brackets taken out, say.
const ENDS = 12;

// Where `sentence` is in `norm`, at or after `after`: [first, last] letter indexes, or null.
function locate(norm: string, sentence: string, after: number): [number, number] | null {
  if (sentence === "") return null;
  const exact = norm.indexOf(sentence, after);
  if (exact >= 0) return [exact, exact + sentence.length - 1];
  if (sentence.length < ENDS * 2) return null;
  const head = sentence.slice(0, ENDS);
  const tail = sentence.slice(-ENDS);
  const start = norm.indexOf(head, after);
  if (start < 0) return null;
  const end = norm.indexOf(tail, start + ENDS);
  // Bounded, so that a common opening and a common ending far apart are not taken for one
  // sentence with half the answer in its middle.
  if (end < 0 || end + ENDS - start > sentence.length * 2 + 40) return null;
  return [start, end + ENDS - 1];
}

function toneOf(sentence: Sentence, played: number): Tone {
  if (sentence.dropped) return "filtered";
  if (sentence.at !== null && played >= sentence.at) return "spoken";
  if (sentence.seconds !== null) return "voiced";
  return "pending";
}

function push(spans: Span[], text: string, tone: Tone) {
  if (text === "") return;
  const previous = spans[spans.length - 1];
  if (previous && previous.tone === tone) previous.text += text;
  else spans.push({ text, tone });
}

const hasWords = (text: string) => /[\p{L}\p{N}]/u.test(text);

export function spans(turn: AgentTurn): Span[] {
  const text = turn.text !== "" ? turn.text : turn.sentences.map((s) => s.text).join(" ");
  if (!turn.aloud) return text === "" ? [] : [{ text, tone: "plain" }];

  // No text came for these fragments; show them as they are.
  if (turn.text === "") {
    const out: Span[] = [];
    turn.sentences.forEach((sentence, i) => push(out, (i ? " " : "") + sentence.text, toneOf(sentence, turn.played)));
    return out;
  }

  const { norm, from } = normalise(text);
  const out: Span[] = [];
  let position = 0; // in `text`
  let after = 0; // in `norm`
  for (const sentence of turn.sentences) {
    const found = locate(norm, normalise(sentence.text).norm, after);
    // A sentence that cannot be found is left out of the colouring rather than guessed at.
    if (!found) continue;
    const start = from[found[0]];
    let end = from[found[1]] + 1;
    // The sentence's own closing punctuation belongs to it: a full stop, a closing quote.
    while (end < text.length && !/[\s\p{L}\p{N}]/u.test(text[end])) end += 1;
    const tone = toneOf(sentence, turn.played);
    const gap = text.slice(position, start);
    // Text between two sentences the splitter let through is text the filter took out: the
    // splitter works in order, so it will never become a sentence. Spacing and markup between
    // them is not words, and goes with what follows.
    push(out, gap, hasWords(gap) ? "filtered" : tone);
    push(out, text.slice(start, end), tone);
    position = end;
    after = found[1] + 1;
  }

  const rest = text.slice(position);
  if (!hasWords(rest)) {
    const previous = out[out.length - 1];
    push(out, rest, previous ? previous.tone : "pending");
  } else {
    // Not found yet is not the same as never: until the answer has settled, what is left may
    // still become a sentence.
    push(out, rest, turn.settled && !turn.writing ? "filtered" : "pending");
  }
  return out;
}
