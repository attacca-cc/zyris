import type { ReactNode } from "react";
import { cn } from "@/lib/utils";
import type { Span, Tone } from "./colors";

// An answer's markdown, drawn without losing the colour each sentence is read in.
//
// **Why not a markdown library**: the colours are a property of characters of the raw text — which
// sentence each one belongs to and how far the speaker has got — and a parser hands back a tree
// with the markers gone and no way back to those positions. This keeps the raw text and marks,
// per character, what is syntax to hide and what is bold or code, so a run of characters can carry
// both its tone and its style. It covers what agents write in a chat: paragraphs, headings,
// lists, quotes, code blocks, **bold**, `code` and [links](url). Tables and the rest stay as text.

type Mark = { hidden: boolean; bold: boolean; code: boolean };
type Block =
  | { kind: "p" | "h" | "quote"; lines: [number, number][] }
  | { kind: "ul" | "ol"; items: [number, number][] }
  | { kind: "code"; lines: [number, number][] };

function inline(text: string, start: number, end: number, marks: Mark[]) {
  const line = text.slice(start, end);
  const hide = (from: number, length: number) => {
    for (let i = from; i < from + length; i += 1) marks[start + i].hidden = true;
  };
  const style = (from: number, to: number, key: "bold" | "code") => {
    for (let i = from; i < to; i += 1) marks[start + i][key] = true;
  };
  const taken = new Array<boolean>(line.length).fill(false);
  const claim = (from: number, to: number) => {
    if (taken.slice(from, to).some(Boolean)) return false;
    taken.fill(true, from, to);
    return true;
  };
  for (const m of line.matchAll(/`([^`]+)`/g)) {
    const at = m.index ?? 0;
    if (!claim(at, at + m[0].length)) continue;
    hide(at, 1);
    hide(at + m[0].length - 1, 1);
    style(at + 1, at + m[0].length - 1, "code");
  }
  for (const m of line.matchAll(/(\*\*|__)(?=\S)(.+?)(?<=\S)\1/g)) {
    const at = m.index ?? 0;
    if (!claim(at, at + m[0].length)) continue;
    hide(at, 2);
    hide(at + m[0].length - 2, 2);
    style(at + 2, at + m[0].length - 2, "bold");
  }
  for (const m of line.matchAll(/\[([^\]]+)\]\(([^)\s]+)\)/g)) {
    const at = m.index ?? 0;
    if (!claim(at, at + m[0].length)) continue;
    hide(at, 1);
    hide(at + 1 + m[1].length, m[0].length - 1 - m[1].length);
  }
}

function parse(text: string): { blocks: Block[]; marks: Mark[] } {
  const marks: Mark[] = Array.from({ length: text.length }, () => ({ hidden: false, bold: false, code: false }));
  const blocks: Block[] = [];
  let fenced = false;
  let at = 0;
  for (const line of text.split("\n")) {
    const start = at;
    const end = at + line.length;
    at = end + 1;
    const last = blocks[blocks.length - 1];
    if (/^\s*```/.test(line)) {
      for (let i = start; i < end; i += 1) marks[i].hidden = true;
      fenced = !fenced;
      if (fenced) blocks.push({ kind: "code", lines: [] });
      continue;
    }
    if (fenced) {
      for (let i = start; i < end; i += 1) marks[i].code = true;
      (last as { lines: [number, number][] }).lines.push([start, end]);
      continue;
    }
    if (line.trim() === "") {
      blocks.push({ kind: "p", lines: [] });
      continue;
    }
    const marker = (length: number) => {
      for (let i = start; i < start + length; i += 1) marks[i].hidden = true;
      inline(text, start + length, end, marks);
      return [start + length, end] as [number, number];
    };
    const heading = /^#{1,6}\s+/.exec(line);
    const bullet = /^\s*[-*+]\s+/.exec(line);
    const numbered = /^\s*\d+[.)]\s+/.exec(line);
    const quote = /^\s*>\s?/.exec(line);
    if (heading) {
      const range = marker(heading[0].length);
      for (let i = range[0]; i < range[1]; i += 1) marks[i].bold = true;
      blocks.push({ kind: "h", lines: [range] });
    } else if (bullet || numbered) {
      const kind = bullet ? "ul" : "ol";
      // A numbered item keeps its number: it is the text, not markup.
      const range = bullet ? marker(bullet[0].length) : marker(0);
      if (last?.kind === kind) last.items.push(range);
      else blocks.push({ kind, items: [range] });
    } else if (quote) {
      const range = marker(quote[0].length);
      if (last?.kind === "quote") last.lines.push(range);
      else blocks.push({ kind: "quote", lines: [range] });
    } else {
      const range = marker(0);
      if (last?.kind === "p" && last.lines.length > 0) last.lines.push(range);
      else blocks.push({ kind: "p", lines: [range] });
    }
  }
  return { blocks: blocks.filter((b) => ("lines" in b ? b.lines.length > 0 : b.items.length > 0)), marks };
}

export function Markdown({
  text,
  spans,
  toneClass,
  trailing,
}: {
  text: string;
  spans: Span[];
  toneClass: Record<Tone, string>;
  // Put at the end of the last block: the writing caret.
  trailing?: ReactNode;
}) {
  const tones: Tone[] = [];
  for (const span of spans) for (let i = 0; i < span.text.length; i += 1) tones.push(span.tone);
  const { blocks, marks } = parse(text);

  const run = ([from, to]: [number, number]): ReactNode[] => {
    const out: ReactNode[] = [];
    let i = from;
    while (i < to) {
      if (marks[i].hidden) {
        i += 1;
        continue;
      }
      const { bold, code } = marks[i];
      const tone = tones[i] ?? "plain";
      let j = i + 1;
      while (j < to && (marks[j].hidden || (marks[j].bold === bold && marks[j].code === code && (tones[j] ?? "plain") === tone))) j += 1;
      const piece = Array.from(text.slice(i, j))
        .filter((_, k) => !marks[i + k]?.hidden)
        .join("");
      out.push(
        <span
          key={i}
          data-tone={tone}
          className={cn(
            toneClass[tone],
            bold && "font-semibold",
            code && "rounded bg-muted px-1 py-px font-mono text-[0.85em]",
          )}
        >
          {piece}
        </span>,
      );
      i = j;
    }
    return out;
  };
  const lines = (ranges: [number, number][]) =>
    ranges.flatMap((range, k) => (k === 0 ? run(range) : [<br key={`br${range[0]}`} />, ...run(range)]));

  return (
    <div className="flex flex-col gap-2.5 text-[0.9375rem] leading-[1.65] [overflow-wrap:anywhere]">
      {blocks.map((block, index) => {
        const end = index === blocks.length - 1 ? trailing : null;
        switch (block.kind) {
          case "h":
            return (
              <p key={index} className="m-0 text-heading">
                {lines(block.lines)}
                {end}
              </p>
            );
          case "quote":
            return (
              <blockquote key={index} className="m-0 border-l-2 border-input pl-3">
                {lines(block.lines)}
                {end}
              </blockquote>
            );
          case "code":
            return (
              <pre key={index} className="m-0 overflow-x-auto rounded-md border bg-background/60 px-3 py-2 font-mono text-[0.8125rem] leading-normal">
                {lines(block.lines)}
                {end}
              </pre>
            );
          case "ul":
          case "ol":
            return (
              <ul key={index} className={cn("m-0 flex flex-col gap-1 pl-5", block.kind === "ul" ? "list-disc" : "list-none pl-0")}>
                {block.items.map((item, k) => (
                  <li key={item[0]}>
                    {run(item)}
                    {k === block.items.length - 1 && end}
                  </li>
                ))}
              </ul>
            );
          default:
            return (
              <p key={index} className="m-0">
                {lines(block.lines)}
                {end}
              </p>
            );
        }
      })}
      {blocks.length === 0 && trailing}
    </div>
  );
}
