import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { MessageCircleIcon } from "lucide-react";
import { SessionPicker } from "./SessionPicker";
import { PHONE, subscribeLevels, subscribeTrace, type Trace } from "./state";
import type { VoiceScreen } from "./Voice";
import { Backdrop, type Mode } from "./conversation/Backdrop";
import { Composer, StatusLine, type Phase } from "./conversation/Composer";
import { fold, fromHistory, type Action, type HistoryLine, type Turn } from "./conversation/fold";
import { perceived } from "./conversation/levels";
import { Thread } from "./conversation/Thread";

// The conversation as it happens: what was said, by whom, and — for the agent's side — how far
// each sentence has got through being made into audio and being heard. See `conversation/fold.ts`
// for how the voice trace becomes turns and `conversation/colors.ts` for the colours.

function asMessage(error: unknown, fallback: string): string {
  return typeof error === "string" ? error : fallback;
}

// Where things stand, for the status line and the backdrop.
function phaseOf(turns: Turn[], speaking: boolean): Phase {
  if (speaking) return "speaking";
  const last = turns[turns.length - 1];
  if (last?.who === "you" && last.state === "recording") return "recording";
  if (last?.who === "you" && last.state === "transcribing") return "transcribing";
  const answer = [...turns].reverse().find((turn) => turn.who === "agent");
  if (answer?.who === "agent" && answer.writing) return "answering";
  return "idle";
}

// Mounted for the life of the window and hidden when another screen is showing, because the turns
// live nowhere else: unmounting it threw the conversation away on every change of screen, and
// missed whatever was said while another screen was open.
export function Conversation({ hidden }: { hidden: boolean }) {
  const [turns, setTurns] = useState<Turn[]>([]);
  // Whether the speaker is playing an answer: from its first sentence queued to the end of it.
  const [speaking, setSpeaking] = useState(false);
  // Whether the push-to-talk key is held, which lights the backdrop before the recording begins.
  const [keyDown, setKeyDown] = useState(false);
  const [screen, setScreen] = useState<VoiceScreen | null>(null);
  const [agentName, setAgentName] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const level = useRef(0);
  const mode = useRef<Mode>("idle");

  const dispatch = useCallback((action: Action) => setTurns((turns) => fold(turns, action)), []);

  // What the session already holds, put in front of anything said since the screen opened. An
  // answer for a session the picker has since left is dropped by its ticket. Read again the next
  // time the screen is shown if it could not be read — before the machine has connected, say.
  const historyTicket = useRef(0);
  const historyRead = useRef(false);
  const readHistory = useCallback(() => {
    const ticket = ++historyTicket.current;
    invoke<{ session: string | null; lines: HistoryLine[] }>("conversation_history")
      .then((history) => {
        if (ticket !== historyTicket.current) return;
        historyRead.current = true;
        const earlier = fromHistory(history?.lines ?? []);
        if (earlier.length > 0) setTurns((live) => [...earlier, ...live]);
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    let stops: (() => void)[] = [];
    let gone = false;
    const onStep = (step: Trace) => {
      if (step.step === "key") setKeyDown(step.down);
      if (step.step === "queued") setSpeaking(true);
      if (step.step === "spoke" || step.step === "interrupted") setSpeaking(false);
      if (step.step === "recording" && step.started) setSpeaking(false);
      dispatch(step);
    };
    void Promise.all([
      subscribeTrace(onStep),
      subscribeLevels((next) => {
        // Only the source the backdrop is showing moves it.
        const wanted = mode.current === "speaking" ? "speaker" : "microphone";
        if (next.source === wanted) level.current = perceived(next.rms);
      }),
    ]).then((unlisten) => {
      if (gone) unlisten.forEach((stop) => stop());
      else stops = unlisten;
    });
    return () => {
      gone = true;
      stops.forEach((stop) => stop());
    };
  }, [dispatch]);

  // What the composer's switches show. Read whenever the screen is shown, since the Voice screen
  // moves the same switches.
  const readVoice = useCallback(() => {
    invoke<VoiceScreen>("voice_state")
      .then(setScreen)
      .catch(() => setScreen(null));
  }, []);
  useEffect(() => {
    if (!hidden) readVoice();
    if (!hidden && !historyRead.current) readHistory();
  }, [hidden, readVoice, readHistory]);

  const phase = phaseOf(turns, speaking);
  const recording = phase === "recording" || keyDown;
  mode.current = speaking ? "speaking" : recording ? "listening" : "idle";

  const voice = screen?.voice ?? null;
  const canHear = voice?.support.state === "ready";
  const listening = canHear ? voice.listening.state === "on" || voice.listening.state === "starting" : null;
  const trigger = screen?.hotkey.state === "working" ? screen.hotkey.trigger : null;
  const readAloud = voice && voice.speaking.state !== "notHere" ? voice.readAloud : null;

  function send(text: string) {
    setProblem(null);
    dispatch({ step: "typed", text });
    invoke("send_conversation_text", { text }).catch((error: unknown) =>
      dispatch({ step: "typedFailed", text, reason: asMessage(error, "The message could not be sent.") }),
    );
  }

  function move(command: string, args: Record<string, unknown>) {
    setProblem(null);
    invoke<VoiceScreen>(command, args)
      .then(setScreen)
      .catch((error: unknown) => setProblem(asMessage(error, "That did not work.")));
  }

  return (
    <main className="flex min-w-0 flex-1 flex-col" hidden={hidden}>
      <SessionPicker
        hidden={hidden}
        onAgentName={setAgentName}
        onSwitched={() => {
          setTurns([]);
          setSpeaking(false);
          readHistory();
        }}
      />

      <div className="relative flex min-h-0 flex-1 flex-col">
        <Backdrop mode={mode.current} level={level} paused={hidden} />

        {turns.length === 0 ? (
          <div className="relative flex flex-1 flex-col items-center justify-center gap-2 px-6 text-center">
            <MessageCircleIcon className="size-6 text-subtle" aria-hidden="true" />
            <p className="m-0 text-[0.9375rem] font-medium text-heading">Start a conversation</p>
            <p className="m-0 max-w-sm text-[0.8125rem] text-muted-foreground">
              Type a message below, or talk to the agent with the push-to-talk key or the wake word.
            </p>
          </div>
        ) : (
          <Thread turns={turns} agentName={agentName ?? "Agent"} />
        )}

        <div className="relative flex shrink-0 flex-col gap-2 px-6 pb-4.5 max-sm:px-3 max-sm:pb-3">
          <div className="mx-auto flex w-full max-w-[45rem] flex-col gap-2">
            <StatusLine
              phase={phase}
              trigger={trigger}
              listening={listening === true}
              readAloud={readAloud !== false}
              onStop={() => void invoke("stop_speaking").catch(() => {})}
            />
            {PHONE && listening === true && <HoldToTalk />}
            <Composer
              onSend={send}
              readAloud={readAloud}
              onReadAloud={(on) => move("set_read_aloud", { readAloud: on })}
              listening={listening}
              recording={recording}
              onMicrophone={(on) => move("set_voice_listening", { listening: on })}
            />
            {problem && (
              <p role="alert" className="m-0 text-center text-xs text-[#ef7d75]">
                {problem}
              </p>
            )}
          </div>
        </div>
      </div>
    </main>
  );
}

// A phone has no push-to-talk key, so this is it: held down, it records; let go, it sends. Every
// way a finger can leave the button ends the turn, so a turn never stays open by accident.
function HoldToTalk() {
  const [down, setDown] = useState(false);
  const press = (next: boolean) => {
    if (next === down) return;
    setDown(next);
    void invoke("push_to_talk", { down: next }).catch(() => {});
  };
  return (
    <button
      type="button"
      className={
        "h-12 w-full touch-none rounded-xl border text-sm font-medium transition-colors select-none " +
        (down ? "border-primary bg-primary text-primary-foreground" : "bg-card text-heading")
      }
      onPointerDown={(event) => {
        event.currentTarget.setPointerCapture(event.pointerId);
        press(true);
      }}
      onPointerUp={() => press(false)}
      onPointerCancel={() => press(false)}
      onLostPointerCapture={() => press(false)}
      onContextMenu={(event) => event.preventDefault()}
    >
      {down ? "Listening — let go to send" : "Hold to talk"}
    </button>
  );
}
