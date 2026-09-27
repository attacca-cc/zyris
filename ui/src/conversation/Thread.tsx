import { useEffect, useLayoutEffect, useRef } from "react";
import { cn } from "@/lib/utils";
import { spans, type Tone } from "./colors";
import type { AgentTurn, Turn, YouTurn } from "./fold";

const TONE: Record<Tone, string> = {
  pending: "text-tone-pending",
  voiced: "text-tone-voiced",
  spoken: "text-tone-spoken",
  filtered: "text-tone-filtered",
  plain: "text-heading",
};

// What a turn of yours looks like, by how far it has got:
// being transcribed — grey and italic; text back but not sent — grey; sent — white.
function Yours({ turn }: { turn: YouTurn }) {
  const transcribing = turn.state === "recording" || turn.state === "transcribing";
  const words = transcribing ? turn.sofar || "…" : turn.text;
  const meta =
    turn.state === "recording"
      ? "Listening"
      : turn.state === "transcribing"
        ? "Transcribing"
        : turn.state === "said"
          ? "Sending"
          : turn.state === "lost"
            ? null
            : null;
  return (
    <li className="flex flex-col items-end gap-1" data-turn="you" data-state={turn.state}>
      {(words !== "" || turn.state !== "lost") && (
        <div
          className={cn(
            "max-w-[72%] max-sm:max-w-[85%] rounded-[1rem] rounded-br-[0.375rem] border bg-[#1d1814]/85 px-3.5 py-2 text-[0.9375rem] leading-relaxed whitespace-pre-wrap [overflow-wrap:anywhere]",
            turn.state === "sent" ? "text-heading" : "text-muted-foreground",
            transcribing && "italic",
            turn.state === "lost" && "line-through decoration-subtle/60",
          )}
        >
          {words}
        </div>
      )}
      {meta && <span className="text-[0.71875rem] text-subtle">{meta}</span>}
      {turn.state === "lost" && (
        <span className="text-[0.75rem] text-[#ef7d75]">Not sent — {turn.detail}</span>
      )}
    </li>
  );
}

// One line on what the agent is doing while it works — its last note or reasoning title, and how
// many tools it has called — so a long quiet stretch reads as work and not as a hang. Once the
// answer is written only the count stays.
function Activity({ turn }: { turn: AgentTurn }) {
  const tools = turn.tools > 0 ? `${turn.tools} ${turn.tools === 1 ? "tool" : "tools"}` : null;
  if (turn.writing && (turn.activity || tools)) {
    return (
      <span role="status" className="flex min-w-0 items-center gap-1.5 text-[0.78125rem] text-subtle">
        <span aria-hidden="true" className="size-1.5 shrink-0 animate-soft-pulse rounded-full bg-primary" />
        <span className="truncate">{[turn.activity, tools].filter(Boolean).join(" · ")}</span>
      </span>
    );
  }
  if (!turn.writing && tools) return <span className="text-[0.75rem] text-subtle">Used {tools}</span>;
  return null;
}

function Theirs({ turn, agentName }: { turn: AgentTurn; agentName: string }) {
  const parts = spans(turn);
  const empty = parts.length === 0;
  return (
    <li className="flex max-w-[88%] flex-col gap-1" data-turn="agent">
      <span className="text-[0.78125rem] font-medium text-muted-foreground">{agentName}</span>
      <Activity turn={turn} />
      {empty && turn.writing ? (
        <span className="flex gap-1.5 pt-2 pb-1" aria-label="Writing">
          {[0, 0.15, 0.3].map((delay) => (
            <span
              key={delay}
              className="size-1.5 animate-soft-pulse rounded-full bg-muted-foreground"
              style={{ animationDelay: `${delay}s` }}
            />
          ))}
        </span>
      ) : (
        <p className="m-0 text-[0.9375rem] leading-[1.65] whitespace-pre-wrap [overflow-wrap:anywhere]">
          {parts.map((part, i) => (
            <span key={i} className={TONE[part.tone]} data-tone={part.tone}>
              {part.text}
            </span>
          ))}
          {turn.writing && (
            <span
              aria-hidden="true"
              className="ml-0.5 inline-block h-[1.05em] w-0.5 animate-caret bg-foreground align-[-0.2em]"
            />
          )}
        </p>
      )}
      {turn.interrupted && (
        <span className="text-[0.75rem] text-subtle">
          Stopped. What you say next tells the agent where it was cut off.
        </span>
      )}
    </li>
  );
}

// The conversation, following its end unless the person has scrolled up to read.
export function Thread({ turns, agentName }: { turns: Turn[]; agentName: string }) {
  const scroller = useRef<HTMLDivElement | null>(null);
  const following = useRef(true);

  useEffect(() => {
    const element = scroller.current;
    if (!element) return;
    const onScroll = () => {
      following.current = element.scrollHeight - element.scrollTop - element.clientHeight < 48;
    };
    element.addEventListener("scroll", onScroll);
    return () => element.removeEventListener("scroll", onScroll);
  }, []);

  useLayoutEffect(() => {
    const element = scroller.current;
    if (element && following.current) element.scrollTop = element.scrollHeight;
  }, [turns]);

  return (
    <div ref={scroller} className="relative min-h-0 flex-1 overflow-y-auto">
      <ol className="mx-auto m-0 flex max-w-[45rem] list-none flex-col gap-5 px-6 pt-6 pb-4 max-sm:px-4">
        {turns.map((turn, at) =>
          turn.who === "you" ? (
            <Yours key={at} turn={turn} />
          ) : turn.who === "agent" ? (
            <Theirs key={at} turn={turn} agentName={agentName} />
          ) : (
            <li key={at} role="alert" className="self-center text-[0.8125rem] text-[#ef7d75]">
              {turn.reason}
            </li>
          ),
        )}
      </ol>
    </div>
  );
}
