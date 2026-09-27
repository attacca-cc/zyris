import { useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  AudioLinesIcon,
  CheckIcon,
  CircleIcon,
  CpuIcon,
  DownloadIcon,
  InfoIcon,
  KeyboardIcon,
  MicIcon,
  SpeakerIcon,
  Trash2Icon,
  Volume2Icon,
} from "lucide-react";
import { subscribeVoice, type VoiceEvent } from "./state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Kbd } from "@/components/ui/kbd";
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
    compute: Compute;
    model: ModelView;
    models: SpeechModel[];
    modelEnv: string | null;
    wake: WakeView;
    speaking: Speaking;
    voiceModel: VoiceModelView;
    voiceModelEnv: string | null;
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

// The speeds offered, and the stored one if it is not among them — a hand-edited file must not
// show as some other speed.
function rateChoices(current: number): number[] {
  const offered = [1, 1.1, 1.25, 1.4];
  return offered.includes(current) ? offered : [...offered, current].sort((a, b) => a - b);
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
      {children && <div className="flex flex-col gap-3">{children}</div>}
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
    <span className="inline-flex items-center gap-1">
      {caps.map((cap, i) => (
        <span key={`${cap}-${i}`} className="inline-flex items-center gap-1">
          {i > 0 && <span className="text-subtle">+</span>}
          <Kbd className="h-6 px-2 text-xs">{cap}</Kbd>
        </span>
      ))}
    </span>
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
  // Which control is waiting on the Rust side, so it cannot be pressed twice.
  const [busy, setBusy] = useState<string | null>(null);
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
        // Ask the machine again rather than leaving the control where it was: something may have
        // been done before the refusal.
        return invoke<VoiceScreen>("voice_state")
          .then(setScreen)
          .catch(() => {});
      })
      .finally(() => setBusy(null));
  }

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
  // **No switch where there is no key.** Turning listening on would open a microphone that
  // nothing could ever start a turn on. `needsAKeyBound` is not this case: the key may well
  // already be bound, and Zyris cannot tell either way.
  const aKeyCouldWork = hotkey.state !== "unavailable";
  const badge = listeningBadge(voice.listening);
  const listeningOn = voice.listening.state === "on" || voice.listening.state === "starting";
  const idle = busy !== null;
  const offered = voice.models.filter((m) => m.state.state === "ready" || m.chosen);

  return (
    <Page>
      <PageHeader
        title="Voice"
        description="Talk to your agent and hear it answer. What you say is turned into text on this computer — the audio is not sent anywhere."
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
                    The microphone <Mono>{voice.listening.device}</Mono> is open. Nothing is recorded
                    until you hold the push-to-talk key or say the wake word.
                  </>
                )
              : voice.listening.state === "starting"
                ? voice.listening.detail
                : voice.listening.state === "off"
                  ? "Nothing is listening. Zyris opens no microphone until you turn this on."
                  : undefined
        }
        action={
          canHear && (
            <div className="flex items-center gap-3">
              <Badge variant={badge.variant}>{badge.label}</Badge>
              {aKeyCouldWork && (
                <Switch
                  aria-label="Listening"
                  checked={listeningOn}
                  disabled={idle}
                  onCheckedChange={(on) => act("listening", "set_voice_listening", { listening: on })}
                />
              )}
            </div>
          )
        }
      >
        {!canHear && <Problem>{(voice.support as { reason: string }).reason}</Problem>}
        {canHear && voice.listening.state === "failed" && <Problem>{voice.listening.reason}</Problem>}
        {canHear && !aKeyCouldWork && (
          <Problem>
            There is no push-to-talk key on this desktop, so a microphone opened here could never be
            asked to record anything. See below.
          </Problem>
        )}
        {refused.listening && <Problem>{refused.listening}</Problem>}
        {last && <Note>{heardLine(last)}</Note>}
      </Section>

      <Section
        icon={<KeyboardIcon />}
        title="How to start talking"
        description="Either one starts a turn. Pressing the key while the agent speaks interrupts it."
      >
        <div className="grid grid-cols-2 gap-3 max-[900px]:grid-cols-1">
          <Panel>
            <span className="text-[0.78125rem] text-muted-foreground">Push-to-talk key</span>
            {hotkey.state === "working" && (
              <>
                <p className="m-0 flex flex-wrap items-center gap-1.5 text-sm text-heading">
                  Hold <Keys trigger={hotkey.trigger} /> to talk, from any window.
                </p>
                {/* The caveat belongs to the backend, not to whether a key is bound. */}
                {!hotkey.releaseConfirmed && (
                  <Note>
                    It has not been confirmed that letting go of the key reaches Zyris on this kind of
                    desktop. If a turn does not end when you let go, Zyris ends it and throws the
                    recording away.
                  </Note>
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
                    <Note>
                      That is the line for {hotkey.desktop}. The shortcut it points at is called{" "}
                      <Mono>{hotkey.shortcutId}</Mono>.
                    </Note>
                  </>
                ) : (
                  <Note>
                    Zyris does not know how {hotkey.desktop} spells that line, so it shows none. Bind a
                    key to the global shortcut named <Mono>{hotkey.shortcutId}</Mono> in your desktop's
                    keyboard settings.
                  </Note>
                )}
                <Note>
                  Zyris cannot tell whether you have bound a key — your desktop does not say. It also
                  has not been confirmed that letting go of the key reaches Zyris here; if a turn does
                  not end when you let go, Zyris ends it and throws the recording away.
                </Note>
              </>
            )}
            {hotkey.state === "unavailable" && (
              <Problem>{hotkey.reason} Nothing on this screen can start a turn until that changes.</Problem>
            )}
          </Panel>

          <Panel>
            <span className="text-[0.78125rem] text-muted-foreground">Wake word</span>
            {voice.wake.state.state === "notHere" ? (
              <Problem>{voice.wake.state.reason}</Problem>
            ) : (
              <>
                {/* The sentence that must not drift, carried from the Rust side. */}
                <Note className="text-foreground">{voice.wake.note}</Note>
                {voice.wake.state.state === "unreadable" && (
                  <Problem>
                    Zyris could not read what has been recorded, so it cannot say how much of the wake
                    word is there. {voice.wake.state.reason}
                  </Problem>
                )}
                {voice.wake.state.state === "nothing" && (
                  <Note>Nothing has been recorded. {voice.wake.wanted} takes are wanted.</Note>
                )}
                {voice.wake.state.state === "partial" && (
                  <Takes recorded={voice.wake.state.recorded} wanted={voice.wake.wanted}>
                    {voice.wake.state.recorded} of {voice.wake.wanted} takes recorded.
                  </Takes>
                )}
                {voice.wake.state.state === "complete" && (
                  <Takes recorded={voice.wake.state.recorded} wanted={voice.wake.wanted}>
                    All {voice.wake.state.recorded} takes are recorded. To record again, clear these
                    first.
                  </Takes>
                )}
                <div className="flex flex-wrap gap-2">
                  {canHear && voice.wake.state.state !== "complete" && (
                    <Button size="sm" disabled={idle} onClick={() => act("wake", "record_wake_take")}>
                      <CircleIcon className="size-3 fill-current" />
                      {busy === "wake" ? "Recording — say it now" : "Record a take"}
                    </Button>
                  )}
                  {(voice.wake.state.state === "partial" || voice.wake.state.state === "complete") && (
                    <Button size="sm" variant="outline" disabled={idle} onClick={() => act("wakeClear", "clear_wake_word")}>
                      {busy === "wakeClear" ? "Clearing" : "Clear them"}
                    </Button>
                  )}
                </div>
                {refused.wake && <Problem>{refused.wake}</Problem>}
                {refused.wakeClear && <Problem>{refused.wakeClear}</Problem>}
                {canHear && (
                  <Note>
                    Recording starts when you press the button and stops when you stop speaking, or
                    after {voice.wake.seconds} seconds.
                  </Note>
                )}
              </>
            )}
          </Panel>
        </div>
      </Section>

      <Section
        icon={<SpeakerIcon />}
        title="Microphone and speaker"
        description="Changing either reopens the microphone and the speaker together."
      >
        <div className="grid grid-cols-2 gap-3 max-[900px]:grid-cols-1">
          <Field label="Microphone">
            <DevicePicker
              list={voice.devices}
              chosen={voice.chosen}
              noun="microphone"
              disabled={idle}
              onChoose={(device) => act("device", "set_voice_device", { device })}
            />
            {refused.device && <Problem>{refused.device}</Problem>}
          </Field>
          <Field label="Speaker">
            <DevicePicker
              list={voice.speakers}
              chosen={voice.speaker}
              noun="speaker"
              disabled={idle}
              onChoose={(speaker) => act("speaker", "set_voice_speaker", { speaker })}
            />
            {refused.speaker && <Problem>{refused.speaker}</Problem>}
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
              <div>
                <Button size="sm" disabled={idle} onClick={() => act("voiceModel", "fetch_voice_model")}>
                  <DownloadIcon />
                  {busy === "voiceModel" ? "Downloading" : "Download the voice"}
                </Button>
              </div>
            ) : (
              <Note>
                <Mono>ZYRIS_TTS_MODELS</Mono> points at <Mono>{voice.voiceModelEnv}</Mono>, so those files
                are yours to manage and Zyris does not download into them.
              </Note>
            )}
            {busy === "voiceModel" && (
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
                While listening is on, answers from session <Mono>{voice.speaking.id}</Mono> are read
                aloud as they are written — with pauses between sentences, since speaking runs behind
                writing. Pressing the key stops it, and what you say next tells the agent where it was
                cut off.
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
                    value={String(voice.speakingRate)}
                    disabled={idle}
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
        description="A larger model understands you better and is slower. Each is downloaded once."
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
              value={voice.models.find((m) => m.chosen)?.id ?? ""}
              disabled={idle}
              onValueChange={(id) => act("model", "set_speech_model", { id })}
              className="gap-2"
            >
              {voice.models.map((model) => (
                <ModelRow
                  key={model.id}
                  model={model}
                  busy={busy}
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
        {busy?.startsWith("model:") && (
          <Note>This takes a few minutes on a slow connection. Nothing is kept until the whole file has arrived and been checked.</Note>
        )}
        {refused.model && <Problem>{refused.model}</Problem>}
        <Note>Choosing one takes effect at once if listening is on, and deleting the one in use turns listening off.</Note>
      </Section>

      {canHear && (
        <Section
          icon={<CpuIcon />}
          title="Performance"
          description="A graphics card is several times faster than the processor. Changing this reloads the model."
        >
          <div className="grid grid-cols-2 gap-3 max-[900px]:grid-cols-1">
            <Field label="Transcribing what you say">
              <Select
                value={voice.compute.transcribeOn}
                disabled={idle}
                onValueChange={(transcribe) => act("compute", "set_voice_compute", { transcribe, speak: null })}
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
                disabled={idle}
                onValueChange={(speak) => act("compute", "set_voice_compute", { transcribe: null, speak })}
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
        What you say is sent to the agent in this computer's Attacca session as text. No recording is
        kept once it has been turned into text.
      </p>
    </Page>
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
  refused,
  onFetch,
  onForget,
}: {
  model: SpeechModel;
  busy: string | null;
  refused: string | undefined;
  onFetch: () => void;
  onForget: () => void;
}) {
  const ready = model.state.state === "ready";
  const mine = busy === `model:${model.id}`;
  return (
    <div
      className={cn(
        "flex flex-col gap-2 rounded-lg border px-3.5 py-3",
        model.chosen ? "border-primary/70 bg-primary/5" : "border-sidebar-border bg-inset",
      )}
    >
      <div className="flex items-center gap-3">
        {/* Only a model on disk can be chosen; the chosen one stays marked even when it is not. */}
        <RadioGroupItem value={model.id} aria-label={model.name} disabled={!ready && !model.chosen} />
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="text-sm font-medium text-heading">
            {model.name} <span className="font-normal text-muted-foreground">· {megabytes(model.bytes)}</span>
          </span>
          <span className="text-[0.78125rem] text-muted-foreground">{model.note}</span>
        </div>
        {ready && model.chosen && (
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
            disabled={busy !== null}
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
            disabled={busy !== null}
            onClick={onFetch}
          >
            <DownloadIcon />
            {mine ? "Downloading" : "Download and use"}
          </Button>
        )}
      </div>
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
