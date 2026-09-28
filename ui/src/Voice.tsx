import { Children, useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AudioLinesIcon,
  CheckIcon,
  CircleIcon,
  CpuIcon,
  DownloadIcon,
  InfoIcon,
  KeyboardIcon,
  LoaderCircleIcon,
  MicIcon,
  SpeakerIcon,
  Trash2Icon,
  Volume2Icon,
} from "lucide-react";
import { PHONE, subscribeVoice, type VoiceEvent } from "./state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Kbd } from "@/components/ui/kbd";
import { Progress } from "@/components/ui/progress";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { IconTile, Mono, Note, Problem } from "@/components/IconTile";
import { Page, PageHeader } from "@/components/PageHeader";
import { keyCaps } from "@/conversation/Composer";
import { cn } from "@/lib/utils";

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
  // `zyris_voice::wake::WHAT_THE_TAKES_DO`, carried rather than written again here. That
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
  // Not yet, rather than not going to: a session is made on the first connection.
  | { state: "noSessionYet" }
  // The account's agents are why there is none — either no agent to create against, or
  // several, which is a choice Zyris does not make.
  | { state: "noAgent"; agents: string[]; settings: string }
  | { state: "session"; id: string };

// `zyris_voice::view::VoiceModelView`. A card of its own rather than two more arms on
// `Speaking`, because a session and the voice are independent: a machine can have one without
// the other, and an enum holding both would have to be silent about whichever it was not.
// `incomplete` carries only what is *left* to fetch, not what the whole snapshot costs.
type VoiceModelView =
  | { state: "ready"; dir: string }
  | { state: "incomplete"; dir: string; missing: number; bytes: number }
  | { state: "unreadable"; dir: string; reason: string }
  | { state: "nowhere"; reason: string }
  | { state: "notHere"; reason: string };

// `view::ComputeView`: where each model can run and where it does. Two lists because whisper can
// be pointed at any one GPU and the voice only at "the GPU".
type ComputeOption = { id: string; name: string };
type Compute = {
  transcribe: ComputeOption[];
  transcribeOn: string;
  speak: ComputeOption[];
  speakOn: string;
};

// `view::SpeechModelView`: one speech model on offer, and what is in the cache for it.
type SpeechModel = {
  id: string;
  name: string;
  note: string;
  bytes: number;
  state: ModelView;
  chosen: boolean;
};

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
    speakers: DeviceList;
    speaker: Choice;
    // A multiple of the voice's own pace, already clamped by the Rust side.
    speakingRate: number;
    // Whether answers are read aloud while listening is on. The Conversation screen's switch.
    readAloud: boolean;
    // A gain on the voice: 1 is as the model writes it.
    volume: number;
    // How much the microphone is amplified, 1 as the device delivers it.
    inputGain: number;
    // The phrase the wake word listens for.
    wakePhrase: string;
    compute: Compute;
    model: ModelView;
    models: SpeechModel[];
    modelEnv: string | null;
    wake: WakeView;
    speaking: Speaking;
    voiceModel: VoiceModelView;
    voiceModelEnv: string | null;
    // Downloads under way: `model:<id>` for a speech model, `voice` for the voice's files.
    downloads?: { id: string; received: number; total: number | null }[];
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
      return "a loudspeaker: it records what is playing, not your voice";
    case "duplex":
      return "a speaker with a microphone: it may pick up what is playing";
    case "unknown":
      return "the sound system will not say which it is without opening it";
  }
}

// The short word beside the switch.
function listeningBadge(listening: Listening): {
  label: string;
  variant: "success" | "secondary" | "destructive";
} {
  switch (listening.state) {
    case "on":
      return { label: "listening", variant: "success" };
    case "starting":
      return { label: "starting", variant: "secondary" };
    case "off":
      return { label: "off", variant: "secondary" };
    case "failed":
      return { label: "not listening", variant: "destructive" };
  }
}

// What the session last said, in one line. No clock behind it: it changes only when the session
// says something else.
function heardLine(event: VoiceEvent): string {
  switch (event.kind) {
    case "listening":
      return "Recording. Let go of the key when you have finished.";
    case "thinking":
      return "Working out what you said.";
    case "heard":
      return `Heard “${event.text}”`;
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

// A gain as a percentage: how loud answers are read, or how much the microphone is amplified.
// Moves freely under the finger and is sent once it is let go, so a drag is one setting rather than
// forty. Never down to 0: drawn too small, one click saved 0 and every answer went silent, which
// reads as the voice being broken rather than turned down.
function LevelSlider({
  label,
  describes,
  value,
  min,
  max,
  disabled,
  onCommit,
}: {
  label: string;
  describes: string;
  value: number;
  min: number;
  max: number;
  disabled: boolean;
  onCommit: (value: number) => void;
}) {
  const [shown, setShown] = useState(Math.round(value * 100));
  useEffect(() => setShown(Math.round(value * 100)), [value]);
  const commit = () => {
    if (shown !== Math.round(value * 100)) onCommit(shown / 100);
  };
  return (
    <Field label={`${label} — ${shown}%`}>
      <input
        type="range"
        className="w-full accent-primary disabled:opacity-50"
        aria-label={describes}
        min={min}
        max={max}
        step={5}
        value={shown}
        disabled={disabled}
        onChange={(event) => setShown(Number(event.target.value))}
        onPointerUp={commit}
        onKeyUp={commit}
      />
    </Field>
  );
}

// The speeds offered, and the stored one if it is not among them — a hand-edited file must not
// show as some other speed.
//
// Rounded first: the rate is an `f32` on the Rust side, so 1.4 arrives as 1.399999976 and was
// shown as a fifth speed of its own beside 1.4.
function rateChoices(current: number): number[] {
  const offered = [1, 1.1, 1.25, 1.4];
  const rate = roundRate(current);
  return offered.includes(rate) ? offered : [...offered, rate].sort((a, b) => a - b);
}

function roundRate(rate: number): number {
  return Math.round(rate * 100) / 100;
}

// Which kind of device an entry is. **On many computers two entries carry the same name**, one of
// them a loudspeaker's monitor, so this is what tells them apart.
function kindOf(device: InputDevice): string {
  switch (device.direction) {
    case "input":
      return "microphone";
    case "output":
      return "loudspeaker monitor";
    case "duplex":
      return "loudspeaker and microphone";
    case "unknown":
      return "kind unknown";
  }
}

function choiceValue(choice: Choice): string {
  return choice.kind === "default" ? "default:" : `device:${choice.id}`;
}

function choiceFrom(value: string): Choice {
  return value === "default:" ? { kind: "default" } : { kind: "device", id: value.slice("device:".length) };
}

// A card with an icon, a title and a line under it.
function Section({
  icon,
  title,
  description,
  action,
  children,
}: {
  icon: ReactNode;
  title: string;
  description?: ReactNode;
  action?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <Card>
      <CardHeader className="items-center">
        <IconTile>{icon}</IconTile>
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <CardTitle>{title}</CardTitle>
          {description && <CardDescription>{description}</CardDescription>}
        </div>
        {action}
      </CardHeader>
      {/* Only when something is in it: a card of conditional lines that are all off is just its
          header, not a header over an empty gap. */}
      {Children.toArray(children).length > 0 && <div className="flex flex-col gap-3">{children}</div>}
    </Card>
  );
}

// An inset panel inside a card.
function Panel({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cn("flex flex-col gap-2.5 rounded-lg border border-sidebar-border bg-inset px-4 py-3.5", className)}>
      {children}
    </div>
  );
}

// A label above a control.
function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <span className="text-[0.78125rem] text-muted-foreground">{label}</span>
      {children}
    </div>
  );
}

// The key as key caps, joined the way the desktop writes them: "Ctrl+Alt+Space".
function Keys({ trigger }: { trigger: string }) {
  const caps = keyCaps(trigger);
  return (
    <span className="mx-0.5 inline-flex items-center gap-1 align-middle whitespace-nowrap">
      {caps.map((cap, i) => (
        <span key={`${cap}-${i}`} className="inline-flex items-center gap-1">
          {i > 0 && <span className="text-subtle">+</span>}
          <Kbd className="h-6 px-2 text-xs">{cap}</Kbd>
        </span>
      ))}
    </span>
  );
}

// A pressed combination as the backend reads it ("Ctrl+Shift+K"), or null while only modifiers
// are down or the key is one a global shortcut should not take on its own.
export function triggerOf(event: Pick<KeyboardEvent, "ctrlKey" | "altKey" | "shiftKey" | "metaKey" | "code">): string | null {
  const code = event.code;
  let key: string | null = null;
  if (/^Key[A-Z]$/.test(code)) key = code.slice(3);
  else if (/^Digit[0-9]$/.test(code)) key = code.slice(5);
  else if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) key = code;
  else if (["Space", "Enter", "Tab", "Backquote", "Minus", "Equal", "Comma", "Period", "Slash", "Semicolon", "Quote",
    "BracketLeft", "BracketRight", "Backslash", "Insert", "Delete", "Home", "End", "PageUp", "PageDown",
    "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Pause", "ScrollLock"].includes(code)) key = code;
  if (key === null) return null;
  const modifiers = [event.ctrlKey && "Ctrl", event.altKey && "Alt", event.shiftKey && "Shift", event.metaKey && "Super"].filter(
    (m): m is string => Boolean(m),
  );
  // A bare letter or space as a global key would swallow it in every other program.
  if (modifiers.length === 0 && !/^F\d+$/.test(key)) return null;
  return [...modifiers, key].join("+");
}

// "Change" on the push-to-talk key: the next combination pressed becomes the key. Escape cancels.
function KeyChanger({ busy, onChoose }: { busy: boolean; onChoose: (trigger: string) => void }) {
  const [recording, setRecording] = useState(false);
  useEffect(() => {
    if (!recording) return;
    const onKey = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (event.code === "Escape") {
        setRecording(false);
        return;
      }
      const trigger = triggerOf(event);
      if (trigger === null) return;
      setRecording(false);
      onChoose(trigger);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, onChoose]);
  return recording ? (
    <div className="flex flex-wrap items-center gap-2">
      <span className="text-[0.8125rem] text-heading animate-soft-pulse">Press the new keys…</span>
      <Button size="sm" variant="ghost" onClick={() => setRecording(false)}>
        Cancel
      </Button>
    </div>
  ) : (
    <div>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => setRecording(true)}>
        Change key
      </Button>
    </div>
  );
}

type Download = { id: string; received: number; total: number | null };

// How far a download has got: a bar, and the megabytes in words for when the bar is too small to
// read. Nothing when nothing is downloading.
function DownloadBar({ download }: { download?: Download }) {
  if (!download) return null;
  const percent = download.total ? Math.min(100, (download.received / download.total) * 100) : null;
  return (
    <div className="flex flex-col gap-1">
      <Progress value={percent ?? 0} aria-label="Download progress" />
      <span className="text-xs text-muted-foreground">
        {megabytes(download.received)}
        {download.total ? ` of ${megabytes(download.total)} · ${Math.floor(percent ?? 0)}%` : " so far"}
      </span>
    </div>
  );
}

function DevicePicker({
  list,
  chosen,
  noun,
  disabled,
  onChoose,
}: {
  list: DeviceList;
  chosen: Choice;
  noun: string;
  disabled: boolean;
  onChoose: (choice: Choice) => void;
}) {
  // Three answers and only one is a list. A sound system that would not answer is not a computer
  // with no devices, and saying it is would be the confident false negative.
  if (list.state === "notHere") return <Problem>{list.reason}</Problem>;
  if (list.state === "unreadable") {
    return (
      <Problem>
        The list of {noun}s could not be read, so there is nothing to choose from here. {list.reason}
      </Problem>
    );
  }
  if (list.devices.length === 0) {
    return <Note>The sound system answered and listed no {noun}s on this computer.</Note>;
  }
  const picked = chosen.kind === "device" ? list.devices.find((d) => d.id === chosen.id) : undefined;
  return (
    <>
      <Select value={choiceValue(chosen)} disabled={disabled} onValueChange={(value) => onChoose(choiceFrom(value))}>
        <SelectTrigger aria-label={`The ${noun}`}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="default:">System default</SelectItem>
          {/* A named device that is no longer listed stays visible as the choice, rather than the
              dropdown silently showing the first entry as if it were chosen. */}
          {chosen.kind === "device" && picked === undefined && (
            <SelectItem value={choiceValue(chosen)}>{chosen.id} (not connected)</SelectItem>
          )}
          {list.devices.map((device) => (
            <SelectItem key={device.id} value={`device:${device.id}`}>
              {device.name} — {kindOf(device)}
              {device.isDefault ? " (default)" : ""}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {picked
        ? picked.direction !== "input" && (
            <Note>
              {picked.isDefault ? "This computer's default, and " : "This is "}
              {whatItIs(picked)}.
            </Note>
          )
        : chosen.kind === "device" && <Problem>That device is not connected now.</Problem>}
    </>
  );
}

export function Voice() {
  const [screen, setScreen] = useState<VoiceScreen | undefined>(undefined);
  const [problem, setProblem] = useState<string | null>(null);
  // Which controls are waiting on the Rust side. Each is kept from being pressed twice; the others
  // stay usable, because the machine takes the requests one at a time on its own.
  const [busy, setBusy] = useState<ReadonlySet<string>>(new Set());
  const isBusy = (name: string) => busy.has(name);
  const fetching = [...busy].some((name) => name.startsWith("model:"));
  // The model asked for, while listening restarts on it. Loading a large model onto a graphics
  // card takes seconds, and the list should say which one is on its way rather than look stuck.
  const [switchingTo, setSwitchingTo] = useState<string | null>(null);
  // What each control was refused, beside that control.
  const [refused, setRefused] = useState<Record<string, string>>({});
  const [last, setLast] = useState<VoiceEvent | null>(null);

  useEffect(() => {
    // StrictMode runs this twice, and both the read and the subscription can land after cleanup.
    let cancelled = false;

    function read() {
      void invoke<VoiceScreen>("voice_state")
        .then((answer) => {
          if (cancelled) return;
          setScreen(answer);
          setProblem(null);
        })
        .catch((error: unknown) => {
          if (cancelled) return;
          // Never an empty machine on a failed read: a read that did not come back says nothing.
          setProblem(asMessage(error, "Could not read what this computer can hear with."));
        });
    }

    read();

    let unlisten: (() => void) | undefined;
    void subscribeVoice((event) => {
      if (cancelled) return;
      setLast(event);
      // A failure can change what is true of the machine — a microphone gone — so read again. An
      // ordinary turn cannot, and reading on every one would be a round trip per sentence.
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

  // What the loading bar says for each action that reloads a model or reopens a device.
  const [loading, setLoading] = useState<Record<string, string>>({});

  function act(name: string, command: string, args?: Record<string, unknown>, loads?: string) {
    setBusy((all) => new Set(all).add(name));
    if (loads) setLoading((all) => ({ ...all, [name]: loads }));
    setRefused((all) => {
      const { [name]: _gone, ...rest } = all;
      return rest;
    });
    void invoke<VoiceScreen>(command, args)
      .then((answer) => setScreen(answer))
      .catch((error: unknown) => {
        setRefused((all) => ({ ...all, [name]: asMessage(error, "That would not happen.") }));
        // Ask the machine again rather than leaving the control where it was: something may have
        // been done before the refusal.
        return invoke<VoiceScreen>("voice_state")
          .then(setScreen)
          .catch(() => {});
      })
      .finally(() =>
        setBusy((all) => {
          const rest = new Set(all);
          rest.delete(name);
          return rest;
        }),
      )
      .finally(() =>
        setLoading((all) => {
          const { [name]: _done, ...rest } = all;
          return rest;
        }),
      );
  }

  // While something downloads, read the machine twice a second so its bar moves (#31). The
  // command that started it answers only when the file is whole.
  // The first launch fetches Base and the voice by itself (`fetch_defaults`), with no button
  // pressed here, so what the machine reports downloading counts too.
  const inFlight = screen?.voice.downloads ?? [];
  const downloading =
    isBusy("voiceModel") || fetching || Object.keys(loading).length > 0 || inFlight.length > 0;
  useEffect(() => {
    if (!downloading) return;
    const timer = setInterval(() => {
      void invoke<VoiceScreen>("voice_state")
        .then(setScreen)
        .catch(() => {});
    }, 500);
    return () => clearInterval(timer);
  }, [downloading]);

  if (screen === undefined) {
    return (
      <Page>
        <PageHeader title="Voice" />
        {problem ? <Problem>{problem}</Problem> : <Note>Reading what this computer can hear with.</Note>}
      </Page>
    );
  }

  const { voice, hotkey } = screen;
  const canHear = voice.support.state === "ready";
  // **Something has to be able to start a turn** for listening to be worth turning on: a key, the
  // hold-to-talk button a phone has in its place, or the wake word, which can be recorded on any
  // machine that hears at all. Only a desktop with no key and no wake word has nothing, and even
  // there the switch stays: turning listening on is how the wake word gets recorded against.
  const aKeyCouldWork = hotkey.state !== "unavailable";
  const wakeCouldWork = voice.wake.state.state !== "notHere";
  const badge = listeningBadge(voice.listening);
  const listeningOn = voice.listening.state === "on" || voice.listening.state === "starting";
  const offered = voice.models.filter((m) => m.state.state === "ready" || m.chosen);

  return (
    <Page>
      <PageHeader
        title="Voice"
        description="Talk to your agent and hear it answer."
      />

      <LoadingBar
        what={
          Object.values(loading)[0] ??
          (voice.listening.state === "starting" ? voice.listening.detail : null) ??
          downloadingLabel(inFlight)
        }
      />

      {problem && <Problem>{problem}</Problem>}

      <Section
        icon={<MicIcon />}
        title="Listening"
        description={
          !canHear
            ? undefined
            : voice.listening.state === "on"
              ? (
                  <>
                    <Mono>{voice.listening.device}</Mono> is open. It records only on the key or the
                    wake word.
                  </>
                )
              : voice.listening.state === "starting"
                ? voice.listening.detail
                : voice.listening.state === "off"
                  ? "Off. No microphone is open."
                  : undefined
        }
        action={
          canHear && (
            <div className="flex items-center gap-3">
              <Badge variant={badge.variant}>{badge.label}</Badge>
              {(aKeyCouldWork || PHONE || wakeCouldWork) && (
                <Switch
                  aria-label="Listening"
                  checked={listeningOn}
                  disabled={isBusy("listening")}
                  onCheckedChange={(on) => act(
                      "listening",
                      "set_voice_listening",
                      { listening: on },
                      on ? "Opening the microphone and loading the speech model…" : "Closing the microphone…",
                    )}
                />
              )}
            </div>
          )
        }
      >
        {!canHear && <Problem>{(voice.support as { reason: string }).reason}</Problem>}
        {canHear && voice.listening.state === "failed" && <Problem>{voice.listening.reason}</Problem>}
        {canHear && !aKeyCouldWork && !PHONE && !wakeCouldWork && (
          <Problem>
            This desktop has no push-to-talk key and cannot listen for a wake word, so nothing could
            start a recording. See below.
          </Problem>
        )}
        {refused.listening && <Problem>{refused.listening}</Problem>}
        {/* What the session last said is only news while it is listening: with listening off, a
            "Recording" from before would read as still recording. */}
        {last && listeningOn && <Note>{heardLine(last)}</Note>}
      </Section>

      <Section
        icon={<KeyboardIcon />}
        title="How to start talking"
        description="Either starts a turn. The key also interrupts an answer."
      >
        <div className="grid grid-cols-2 gap-3 max-[900px]:grid-cols-1">
          <Panel>
            <span className="text-[0.78125rem] text-muted-foreground">Push-to-talk key</span>
            {hotkey.state === "working" && (
              <>
                <p className="m-0 text-sm leading-7 text-heading">
                  Hold <Keys trigger={hotkey.trigger} /> to talk, from any window.
                </p>
                {/* The caveat belongs to the backend, not to whether a key is bound. */}
                {!hotkey.releaseConfirmed && (
                  <Note>If letting go does not end a turn, Zyris ends it and drops the recording.</Note>
                )}
              </>
            )}
            {hotkey.state === "needsAKeyBound" && (
              <>
                <p className="m-0 text-sm text-heading">{hotkey.how}</p>
                {hotkey.line !== null ? (
                  <>
                    <pre className="m-0 overflow-x-auto rounded-md border bg-background px-3 py-2 font-mono text-xs text-heading">
                      {hotkey.line}
                    </pre>
                  </>
                ) : (
                  <Note>
                    The shortcut is called <Mono>{hotkey.shortcutId}</Mono>.
                  </Note>
                )}
              </>
            )}
            {hotkey.state === "unavailable" &&
              (PHONE ? (
                <p className="m-0 text-sm text-heading">
                  On a phone, hold the <strong>Hold to talk</strong> button on the Conversation screen.
                </p>
              ) : (
                <Problem>
                  {hotkey.reason} {wakeCouldWork ? "Use the wake word instead." : "Nothing here can start a turn."}
                </Problem>
              ))}
            {(hotkey.state === "working" || (hotkey.state === "needsAKeyBound" && hotkey.line !== null)) && (
              <KeyChanger
                busy={isBusy("hotkey")}
                onChoose={(trigger) => act("hotkey", "set_push_to_talk_key", { trigger })}
              />
            )}
            {refused.hotkey && <Problem>{refused.hotkey}</Problem>}
          </Panel>

          <Panel>
            <span className="text-[0.78125rem] text-muted-foreground">Wake word</span>
            {voice.wake.state.state === "notHere" ? (
              <Problem>{voice.wake.state.reason}</Problem>
            ) : (
              <>
                {/* The sentence that must not drift, carried from the Rust side. */}
                <Note className="text-foreground">{voice.wake.note}</Note>
                <WakePhrase
                  phrase={voice.wakePhrase}
                  busy={isBusy("wakePhrase")}
                  onSave={(phrase) => act("wakePhrase", "set_wake_phrase", { phrase })}
                />
                {refused.wakePhrase && <Problem>{refused.wakePhrase}</Problem>}
                {voice.wake.state.state === "unreadable" && (
                  <Problem>The recordings could not be read. {voice.wake.state.reason}</Problem>
                )}
                {voice.wake.state.state === "nothing" && (
                  <Note>No recordings yet; {voice.wake.wanted} are needed.</Note>
                )}
                {voice.wake.state.state === "partial" && (
                  <Takes recorded={voice.wake.state.recorded} wanted={voice.wake.wanted}>
                    {voice.wake.state.recorded} of {voice.wake.wanted} takes recorded.
                  </Takes>
                )}
                {voice.wake.state.state === "complete" && (
                  <Takes recorded={voice.wake.state.recorded} wanted={voice.wake.wanted}>
                    All {voice.wake.state.recorded} takes recorded.
                  </Takes>
                )}
                <div className="flex flex-wrap gap-2">
                  {canHear && voice.wake.state.state !== "complete" && (
                    <Button size="sm" disabled={isBusy("wake")} onClick={() => act("wake", "record_wake_take")}>
                      <CircleIcon className="size-3 fill-current" />
                      {isBusy("wake") ? "Recording — say it now" : "Record a take"}
                    </Button>
                  )}
                  {(voice.wake.state.state === "partial" || voice.wake.state.state === "complete") && (
                    <Button size="sm" variant="outline" disabled={isBusy("wakeClear")} onClick={() => act("wakeClear", "clear_wake_word")}>
                      {isBusy("wakeClear") ? "Clearing" : "Clear them"}
                    </Button>
                  )}
                </div>
                {refused.wake && <Problem>{refused.wake}</Problem>}
                {refused.wakeClear && <Problem>{refused.wakeClear}</Problem>}
                {canHear && (
                  <Note>Each take stops when you stop speaking, or after {voice.wake.seconds} seconds.</Note>
                )}
              </>
            )}
          </Panel>
        </div>
      </Section>

      <Section
        icon={<SpeakerIcon />}
        title="Microphone and speaker"
        description="Changing either reopens both."
      >
        <div className="grid grid-cols-2 gap-3 max-[900px]:grid-cols-1">
          <Field label="Microphone">
            <DevicePicker
              list={voice.devices}
              chosen={voice.chosen}
              noun="microphone"
              disabled={isBusy("device")}
              onChoose={(device) => act("device", "set_voice_device", { device }, "Switching the microphone…")}
            />
            {refused.device && <Problem>{refused.device}</Problem>}
            <LevelSlider
              label="Input gain"
              describes="How much the microphone is amplified"
              value={voice.inputGain}
              min={25}
              max={400}
              disabled={isBusy("gain")}
              onCommit={(gain) => act("gain", "set_input_gain", { gain })}
            />
            {refused.gain && <Problem>{refused.gain}</Problem>}
          </Field>
          <Field label="Speaker">
            <DevicePicker
              list={voice.speakers}
              chosen={voice.speaker}
              noun="speaker"
              disabled={isBusy("speaker")}
              onChoose={(speaker) => act("speaker", "set_voice_speaker", { speaker }, "Switching the speaker…")}
            />
            {refused.speaker && <Problem>{refused.speaker}</Problem>}
            <LevelSlider
              label="Volume"
              describes="How loud answers are read"
              value={voice.volume}
              min={10}
              max={200}
              disabled={isBusy("volume")}
              onCommit={(volume) => act("volume", "set_voice_volume", { volume })}
            />
            {refused.volume && <Problem>{refused.volume}</Problem>}
          </Field>
        </div>
      </Section>

      <Section icon={<Volume2Icon />} title="Reading answers aloud">
        {voice.speaking.state === "notHere" && <Note>{voice.speaking.reason}</Note>}

        {voice.voiceModel.state === "nowhere" && (
          <Problem>The voice cannot be kept anywhere on this computer. {voice.voiceModel.reason}</Problem>
        )}
        {voice.voiceModel.state === "unreadable" && (
          <Problem>
            Something is in the way at <Mono>{voice.voiceModel.dir}</Mono> and it could not be read:{" "}
            {voice.voiceModel.reason}
          </Problem>
        )}
        {voice.voiceModel.state === "incomplete" && (
          <Panel>
            <p className="m-0 text-sm text-heading">
              The voice has not been downloaded yet, so nothing can be read aloud. It is about{" "}
              {megabytes(voice.voiceModel.bytes)} in {voice.voiceModel.missing} file
              {voice.voiceModel.missing === 1 ? "" : "s"}, downloaded once and kept at{" "}
              <Mono>{voice.voiceModel.dir}</Mono>.
            </p>
            {voice.voiceModelEnv === null ? (
              <>
                <div>
                  <Button size="sm" disabled={isBusy("voiceModel")} onClick={() => act("voiceModel", "fetch_voice_model")}>
                    <DownloadIcon />
                    {isBusy("voiceModel") ? "Downloading" : "Download the voice"}
                  </Button>
                </div>
                <DownloadBar download={voice.downloads?.find((d) => d.id === "voice")} />
              </>
            ) : (
              <Note>
                <Mono>ZYRIS_TTS_MODELS</Mono> points at <Mono>{voice.voiceModelEnv}</Mono>, so those files
                are yours to manage and Zyris does not download into them.
              </Note>
            )}
            {isBusy("voiceModel") && (
              <Note>Each file is checked against its digest as it arrives. This takes a few minutes.</Note>
            )}
          </Panel>
        )}
        {refused.voiceModel && <Problem>{refused.voiceModel}</Problem>}

        {voice.speaking.state === "noSessionYet" && (
          <Note>
            Nothing is read aloud yet. A session is made the first time this computer connects to
            Attacca, and answers from it are read out here.
          </Note>
        )}

        {voice.speaking.state === "noAgent" &&
          (voice.speaking.agents.length === 0 ? (
            <Note>
              Nothing is read aloud. A session belongs to an agent and this account has none. Make an
              agent in Attacca and restart Zyris.
            </Note>
          ) : (
            <>
              <Note>
                Nothing is read aloud. This account has {voice.speaking.agents.length} agents —{" "}
                {voice.speaking.agents.join(", ")} — and Zyris will not choose for you.
              </Note>
              <Note>
                Start a session with one of them from the top of the Conversation tab, or put one of
                those names in <Mono>{voice.speaking.settings}</Mono> under <Mono>agent</Mono> and
                restart Zyris.
              </Note>
            </>
          ))}

        {voice.speaking.state === "session" && (
          <>
            {/* Two facts, so two sentences: a session is named *and* the voice is on disk before
                anything is read aloud. */}
            {voice.voiceModel.state === "ready" ? (
              <Note>
                While listening is on, answers are read aloud as they are written, with pauses between sentences.
              </Note>
            ) : (
              <Note>
                Session <Mono>{voice.speaking.id}</Mono> is the one this computer would answer from,
                and nothing is read aloud until the voice above is on disk.
              </Note>
            )}
            {voice.voiceModel.state === "ready" && (
              <div className="max-w-60">
                <Field label="Reading speed">
                  <Select
                    value={String(roundRate(voice.speakingRate))}
                    disabled={isBusy("rate")}
                    onValueChange={(value) => act("rate", "set_speaking_rate", { rate: Number(value) })}
                  >
                    <SelectTrigger aria-label="How fast answers are read">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      {rateChoices(voice.speakingRate).map((rate) => (
                        <SelectItem key={rate} value={String(rate)}>
                          {rate === 1 ? "1× — the voice's own pace" : `${rate}×`}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </Field>
              </div>
            )}
            {refused.rate && <Problem>{refused.rate}</Problem>}
          </>
        )}
      </Section>

      <Section
        icon={<AudioLinesIcon />}
        title="Speech recognition"
        description="Larger is more accurate and slower."
      >
        {voice.model.state === "notHere" && <Problem>{voice.model.reason}</Problem>}
        {voice.model.state === "nowhere" && (
          <Problem>
            {voice.model.reason} Zyris will use a model file you point it at yourself: set{" "}
            <Mono>ZYRIS_WHISPER_MODEL</Mono> to its path.
          </Problem>
        )}
        {voice.model.state === "unreadable" && (
          <Problem>
            There is something at <Mono>{voice.model.path}</Mono> that Zyris could not read, so the
            speech model could not be loaded. {voice.model.reason}
          </Problem>
        )}

        {voice.modelEnv !== null ? (
          <Note>
            The model in use was named by <Mono>ZYRIS_WHISPER_MODEL</Mono> (<Mono>{voice.modelEnv}</Mono>
            ). Zyris takes that file as given: it does not check it, replace it or delete it.
          </Note>
        ) : (
          <>
            <RadioGroup
              aria-label="Speech model"
              value={(isBusy("model") && switchingTo) || (voice.models.find((m) => m.chosen)?.id ?? "")}
              disabled={isBusy("model")}
              onValueChange={(id) => {
                setSwitchingTo(id);
                const name = voice.models.find((m) => m.id === id)?.name ?? id;
                act("model", "set_speech_model", { id }, `Loading ${name}…`);
              }}
              className="gap-2"
            >
              {voice.models.map((model) => (
                <ModelRow
                  key={model.id}
                  model={model}
                  busy={isBusy(`model:${model.id}`)}
                  switching={isBusy("model") && switchingTo === model.id}
                  download={voice.downloads?.find((d) => d.id === `model:${model.id}`)}
                  refused={refused[`model:${model.id}`]}
                  onFetch={() => act(`model:${model.id}`, "fetch_speech_model", { id: model.id })}
                  onForget={() => act(`model:${model.id}`, "forget_speech_model", { id: model.id })}
                />
              ))}
            </RadioGroup>
            {offered.length > 0 && offered.every((m) => m.state.state !== "ready") && (
              <Problem>No model is downloaded yet. Download one above.</Problem>
            )}
          </>
        )}
        {fetching && (
          <Note>This takes a few minutes on a slow connection. Nothing is kept until the whole file has arrived and been checked.</Note>
        )}
        {refused.model && <Problem>{refused.model}</Problem>}
        <Note>Deleting the one in use turns listening off.</Note>
      </Section>

      {canHear && (
        <Section
          icon={<CpuIcon />}
          title="Performance"
          description="A graphics card is several times faster."
        >
          <div className="grid grid-cols-2 gap-3 max-[900px]:grid-cols-1">
            <Field label="Transcribing what you say">
              <Select
                value={voice.compute.transcribeOn}
                disabled={isBusy("compute")}
                onValueChange={(transcribe) => act(
                    "compute",
                    "set_voice_compute",
                    { transcribe, speak: null },
                    movingTo("Transcription", voice.compute.transcribe, transcribe),
                  )}
              >
                <SelectTrigger aria-label="Where speech is transcribed">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {voice.compute.transcribe.map((option) => (
                    <SelectItem key={option.id} value={option.id}>
                      {option.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label="Reading answers aloud">
              <Select
                value={voice.compute.speakOn}
                disabled={isBusy("compute")}
                onValueChange={(speak) => act(
                    "compute",
                    "set_voice_compute",
                    { transcribe: null, speak },
                    movingTo("Reading aloud", voice.compute.speak, speak),
                  )}
              >
                <SelectTrigger aria-label="Where answers are read aloud">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {voice.compute.speak.map((option) => (
                    <SelectItem key={option.id} value={option.id}>
                      {option.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </div>
          {voice.compute.transcribe.length === 1 && voice.compute.speak.length === 1 && (
            <Note>
              This build runs both on the processor. A build with the <Mono>gpu</Mono> feature lists this
              computer's graphics cards here.
            </Note>
          )}
          {refused.compute && <Problem>{refused.compute}</Problem>}
        </Section>
      )}

      <p className="m-0 flex items-start gap-2 rounded-lg border border-sidebar-border px-3.5 py-3 text-[0.8125rem] text-muted-foreground">
        <InfoIcon className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        Speech is turned into text on this computer; only the text is sent, and no audio is kept.
      </p>
    </Page>
  );
}

// The phrase listened for, typed. It has to be what the takes say: "Hey Agent" on a phone, say,
// so the computer beside it listening for "Hey Zyris" does not wake too.
function WakePhrase({ phrase, busy, onSave }: { phrase: string; busy: boolean; onSave: (phrase: string) => void }) {
  const [typed, setTyped] = useState(phrase);
  useEffect(() => setTyped(phrase), [phrase]);
  return (
    <form
      className="flex items-center gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        onSave(typed);
      }}
    >
      <Input
        aria-label="The wake phrase"
        value={typed}
        onChange={(event) => setTyped(event.target.value)}
        className="h-8 flex-1"
      />
      <Button type="submit" size="sm" variant="outline" disabled={busy || typed.trim() === phrase}>
        Save
      </Button>
    </form>
  );
}

// What the bar says while models download, with how far along they are together.
function downloadingLabel(downloads: { received: number; total: number | null }[]): string | null {
  if (downloads.length === 0) return null;
  const received = downloads.reduce((sum, d) => sum + d.received, 0);
  const total = downloads.reduce((sum, d) => sum + (d.total ?? 0), 0);
  const percent = total > 0 ? ` — ${Math.floor((received / total) * 100)}%` : "";
  return `Downloading the speech models${percent}. Speech is ready when this finishes.`;
}

// What the bar says while a model moves between the processor and a graphics card.
function movingTo(what: string, options: { id: string; name: string }[], id: string): string {
  const name = options.find((option) => option.id === id)?.name ?? id;
  const gpu = id.startsWith("gpu") ? " The first time on a graphics card after an update takes up to 20 seconds." : "";
  return `${what} is moving to ${name}.${gpu}`;
}

// **Something is loading, and the screen says so rather than going still.** Whisper and ONNX
// Runtime report no progress while they load, so the bar moves without a percentage; every other
// control stays usable meanwhile.
function LoadingBar({ what }: { what: string | null }) {
  if (!what) return null;
  return (
    <div role="status" className="flex flex-col gap-1.5 rounded-lg border border-primary/40 bg-primary/5 px-3.5 py-3">
      <span className="flex items-center gap-2 text-[0.8125rem] text-heading">
        <LoaderCircleIcon className="size-3.5 animate-spin text-primary" aria-hidden="true" />
        {what}
      </span>
      <div className="relative h-1 w-full overflow-hidden rounded-full bg-border">
        <div className="absolute inset-y-0 left-0 w-2/5 rounded-full bg-primary animate-indeterminate" />
      </div>
    </div>
  );
}

// The wake word's takes, as a row of marks and a sentence.
function Takes({ recorded, wanted, children }: { recorded: number; wanted: number; children: ReactNode }) {
  return (
    <div className="flex items-center gap-2.5">
      <span className="flex gap-1" aria-hidden="true">
        {Array.from({ length: wanted }, (_, i) => (
          <span key={i} className={cn("h-1.5 w-4 rounded-full", i < recorded ? "bg-primary" : "bg-input")} />
        ))}
      </span>
      <Note>{children}</Note>
    </div>
  );
}

// One speech model: choose it, download it, or delete it.
function ModelRow({
  model,
  busy,
  switching,
  download,
  refused,
  onFetch,
  onForget,
}: {
  model: SpeechModel;
  // This model's own download or deletion is waiting on the Rust side.
  busy: boolean;
  switching: boolean;
  download?: Download;
  refused: string | undefined;
  onFetch: () => void;
  onForget: () => void;
}) {
  const ready = model.state.state === "ready";
  const mine = busy;
  return (
    <div
      className={cn(
        "flex flex-col gap-2 rounded-lg border px-3.5 py-3",
        model.chosen || switching ? "border-primary/70 bg-primary/5" : "border-sidebar-border bg-inset",
      )}
    >
      <div className="action-row items-center gap-3">
        {/* Only a model on disk can be chosen; the chosen one stays marked even when it is not. */}
        <RadioGroupItem
          id={`model-${model.id}`}
          value={model.id}
          aria-label={model.name}
          disabled={!ready && !model.chosen}
        />
        {/* A label for the radio, so the name and the note choose the model too, not only the
            small circle beside them. */}
        <label
          htmlFor={`model-${model.id}`}
          className={cn("flex min-w-0 flex-1 flex-col gap-0.5", ready && !model.chosen && "cursor-pointer")}
        >
          <span className="text-sm font-medium text-heading">
            {model.name} <span className="font-normal text-muted-foreground">· {megabytes(model.bytes)}</span>
          </span>
          <span className="text-[0.78125rem] text-muted-foreground">{model.note}</span>
        </label>
        {switching && (
          <span role="status" className="inline-flex items-center gap-1.5 text-xs text-primary">
            <LoaderCircleIcon className="size-3.5 animate-spin" aria-hidden="true" />
            Switching to this model…
          </span>
        )}
        {ready && model.chosen && !switching && (
          <span className="inline-flex items-center gap-1 text-xs text-[#a9c99a]">
            <CheckIcon className="size-3.5" aria-hidden="true" />
            In use
          </span>
        )}
        {ready && (
          <Button
            variant="outline"
            size="icon-sm"
            aria-label={`Delete ${model.name}`}
            title={mine ? "Deleting" : "Delete it from this computer"}
            disabled={busy}
            onClick={onForget}
          >
            <Trash2Icon />
          </Button>
        )}
        {(model.state.state === "absent" || model.state.state === "damaged") && (
          <Button
            variant="outline"
            size="sm"
            aria-label={`Download ${model.name}`}
            title={`Downloaded once and kept at ${model.state.path}`}
            disabled={busy}
            onClick={onFetch}
          >
            <DownloadIcon />
            {mine ? "Downloading" : "Download and use"}
          </Button>
        )}
      </div>
      <DownloadBar download={download} />
      {model.state.state === "damaged" && (
        <Problem>
          The file on disk is {megabytes(model.state.bytes)} and this model is {megabytes(model.state.expected)},
          so it cannot be loaded; downloading replaces it.
        </Problem>
      )}
      {model.state.state === "absent" && model.chosen && (
        <Note>Chosen, and not downloaded yet: listening needs it.</Note>
      )}
      {model.state.state === "unreadable" && (
        <Problem>
          Something at <Mono>{model.state.path}</Mono> could not be read. {model.state.reason}
        </Problem>
      )}
      {refused && <Problem>{refused}</Problem>}
    </div>
  );
}
