import { useState, type FormEvent } from "react";
import { ArrowUpIcon, MicIcon, MicOffIcon, SquareIcon, Volume2Icon, VolumeXIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { cn } from "@/lib/utils";
import { PHONE } from "@/state";

// Where the conversation is, for the line above the text box.
export type Phase = "idle" | "recording" | "transcribing" | "answering" | "speaking";

// A key combination as the desktop names it — `<Control>space`, `Ctrl+Space` — as key caps.
export function keyCaps(trigger: string): string[] {
  const parts = trigger
    .replace(/<([^>]+)>/g, "$1+")
    .split(/[+\s]+/)
    .filter((part) => part !== "");
  return parts.map((part) => {
    const lower = part.toLowerCase();
    if (lower === "control" || lower === "ctrl" || lower === "primary") return "Ctrl";
    if (lower === "super" || lower === "meta" || lower === "logo") return "Super";
    if (lower === "alt") return "Alt";
    if (lower === "shift") return "Shift";
    return part.length === 1 ? part.toUpperCase() : part[0].toUpperCase() + part.slice(1);
  });
}

export function StatusLine({
  phase,
  trigger,
  listening,
  readAloud,
  onStop,
}: {
  phase: Phase;
  // The push-to-talk key, when one is bound.
  trigger: string | null;
  listening: boolean;
  readAloud: boolean;
  onStop: () => void;
}) {
  return (
    <div
      role="status"
      className="flex h-[1.125rem] items-center justify-center gap-1.5 text-xs text-muted-foreground"
    >
      {(phase === "recording" || phase === "transcribing") && (
        <span aria-hidden="true" className="size-1.5 animate-soft-pulse rounded-full bg-primary" />
      )}
      {phase === "idle" &&
        (listening && trigger ? (
          <>
            <span>Hold</span>
            {keyCaps(trigger).map((cap) => (
              <Kbd key={cap}>{cap}</Kbd>
            ))}
            <span>to talk, or type below</span>
          </>
        ) : listening && PHONE ? (
          <span>Hold the button below to talk, or type</span>
        ) : listening ? (
          <span>Say the wake word, or type below</span>
        ) : (
          <span>Type below, or turn on the microphone to talk</span>
        ))}
      {phase === "recording" && <span>Listening — let go to send</span>}
      {phase === "transcribing" && <span>Transcribing</span>}
      {phase === "answering" && (
        <span>{readAloud ? "The agent is answering" : "The agent is answering · not reading aloud"}</span>
      )}
      {phase === "speaking" && (
        <>
          <span>Speaking{trigger ? " — press the key to interrupt" : ""}</span>
          <Button
            variant="outline"
            onClick={onStop}
            className="ml-1 h-5 gap-1 rounded-full px-2 text-[0.71875rem] has-[>svg]:px-2"
          >
            <SquareIcon className="size-2.5 fill-current" aria-hidden="true" />
            Stop
          </Button>
        </>
      )}
    </div>
  );
}

export function Composer({
  onSend,
  readAloud,
  onReadAloud,
  listening,
  recording,
  onMicrophone,
  disabled = false,
}: {
  // Resolves once the message is on its way; the box is cleared at once either way.
  onSend: (text: string) => void;
  // `null` while the voice state has not been read, and on a build that cannot speak.
  readAloud: boolean | null;
  onReadAloud: (on: boolean) => void;
  // Whether the microphone is open (listening is on). `null`: this computer cannot listen.
  listening: boolean | null;
  recording: boolean;
  onMicrophone: (on: boolean) => void;
  disabled?: boolean;
}) {
  const [draft, setDraft] = useState("");
  const text = draft.trim();

  function submit(event: FormEvent) {
    event.preventDefault();
    if (text === "" || disabled) return;
    onSend(text);
    setDraft("");
  }

  return (
    <form
      onSubmit={submit}
      className="flex h-12 items-center gap-1.5 rounded-[0.875rem] border bg-[#15110e]/90 pr-1.5 pl-4 shadow-[0_8px_30px_rgba(0,0,0,0.3)] focus-within:border-input"
    >
      <input
        aria-label="Message"
        placeholder="Type a message…"
        value={draft}
        disabled={disabled}
        onChange={(event) => setDraft(event.target.value)}
        className="h-full min-w-0 flex-1 bg-transparent text-[0.90625rem] text-heading outline-none placeholder:text-subtle disabled:opacity-60"
      />
      {readAloud !== null && (
        <button
          type="button"
          aria-pressed={readAloud}
          title={readAloud ? "Answers are read aloud" : "Answers are not read aloud"}
          onClick={() => onReadAloud(!readAloud)}
          className={cn(
            "inline-flex h-[1.875rem] shrink-0 items-center gap-1.5 rounded-full border px-2.5 text-[0.78125rem] transition-colors outline-none focus-visible:ring-[3px] focus-visible:ring-ring/40",
            readAloud
              ? "border-primary/35 bg-primary/10 text-[#e3a283]"
              : "border-border text-[#8a827b] hover:text-foreground",
          )}
        >
          {readAloud ? (
            <Volume2Icon className="size-[0.9375rem]" aria-hidden="true" />
          ) : (
            <VolumeXIcon className="size-[0.9375rem]" aria-hidden="true" />
          )}
          <span className="max-[560px]:sr-only">Read aloud</span>
        </button>
      )}
      {listening !== null && (
        <button
          type="button"
          aria-pressed={listening}
          aria-label={listening ? "Turn the microphone off" : "Turn the microphone on"}
          title={listening ? "Microphone on" : "Microphone off"}
          onClick={() => onMicrophone(!listening)}
          className={cn(
            "inline-flex size-9 shrink-0 items-center justify-center rounded-[0.625rem] border transition-colors outline-none focus-visible:ring-[3px] focus-visible:ring-ring/40",
            recording
              ? "border-primary bg-primary text-primary-foreground"
              : listening
                ? "border-transparent text-[#bdb5ad] hover:bg-accent"
                : "border-transparent text-subtle hover:bg-accent hover:text-foreground",
          )}
        >
          {listening ? <MicIcon className="size-4" aria-hidden="true" /> : <MicOffIcon className="size-4" aria-hidden="true" />}
        </button>
      )}
      <button
        type="submit"
        aria-label="Send"
        disabled={text === "" || disabled}
        className="inline-flex size-9 shrink-0 items-center justify-center rounded-[0.625rem] bg-heading text-background transition-colors outline-none focus-visible:ring-[3px] focus-visible:ring-ring/40 disabled:bg-secondary disabled:text-subtle"
      >
        <ArrowUpIcon className="size-4" aria-hidden="true" />
      </button>
    </form>
  );
}
