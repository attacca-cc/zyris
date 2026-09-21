import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Mirrors CoreEvent in crates/zyris-runtime/src/event.rs. Serialized there as a tagged union with
// camelCase fields, so this is a transcription rather than a parse.
export type CoreEvent =
  | { kind: "started" }
  | { kind: "shuttingDown" }
  | { kind: "needsEnrolment" }
  | { kind: "enrolmentCode"; userCode: string; verificationUri: string }
  | { kind: "enrolmentFailed"; reason: string }
  | { kind: "connecting" }
  | { kind: "connected"; nodeId: string; nodeName: string }
  | { kind: "disconnected"; reason: string; retrying: boolean }
  | { kind: "setupFailed"; reason: string }
  | { kind: "paused"; paused: boolean }
  | ({ kind: "needsPeerApproval" } & PeerQuestion)
  | ({ kind: "toolCall" } & ToolCall)
  | ({ kind: "mcpServer" } & McpServerEvent);

// What happened to one local MCP server. Mirrors `McpServerChange` in
// crates/zyris-runtime/src/event.rs — internally tagged on `change`, so this switches on one
// field and each variant's own detail travels beside it.
//
// **`disabled` and `died` are two answers and not one.** An agent that finds the capability gone
// cannot tell them apart and does not need to; the person at the window can, and one of the two
// is a process to restart. The core keeps them apart all the way from `ServerState`, and the MCP
// screen is the last place that can throw the distinction away.
export type McpServerChange =
  | { change: "announced"; capability: string; tools: number }
  | { change: "disabled" }
  | { change: "died" }
  | { change: "failed"; reason: string };

export type McpServerEvent = {
  server: string;
  change: McpServerChange;
};

// A machine this computer is about to send a file to and has never sent to before, waiting for a
// person to say yes or no. `id` names the question an answer has to name back — the core refuses
// an answer that names a question no longer waiting, which is what stops a stale window or a
// second click from approving something nobody was looking at.
//
// `fingerprint` is upstream's rendering of that machine's key: 128 bits as eight space-separated
// groups of four uppercase hex digits. It is carried and shown exactly as given — the person is
// comparing it against another screen character by character, and anything that re-cases,
// re-groups or truncates it makes that comparison fail for a reason that has nothing to do with
// the keys. crates/zyris-app/src/bridge.rs pins these field names from the Rust side.
export type PeerQuestion = {
  id: number;
  label: string;
  fingerprint: string;
};

// One call an agent made. `outcome` is "allowed", "refused" or "failed", spelled the same way in
// a live event and in a stored line of the audit log — Outcome::as_str and its serde form in
// crates/zyris-tools/src/audit.rs, which a test there keeps in agreement.
export type ToolCall = {
  capability: string;
  tool: string;
  detail: string;
  outcome: string;
};

// A call as the Tools screen lists it. The stored lines the `recent_tool_calls` command returns
// already have this shape — `zyris_tools::Entry` — and a live event is given the same one by the
// reducer, which stamps it as it arrives. See the `toolCall` arm for what that time means.
export type ToolCallRow = ToolCall & { at: string };

// The screens a person can move between once this machine is enrolled, and what the sidebar calls
// each of them. `starting` and `onboarding` are not among them: they are where the core puts the
// window, not where anyone chooses to be.
//
// **The list is the definition and `Tab` is derived from it**, rather than the two being written
// out separately and kept in step by hand. A screen that exists and is not in this list is a
// screen with nothing to navigate to it — reachable only by an event, which for a tab is never —
// and that failure is silent in a way nothing here would catch.
export const TABS = [
  { id: "status", label: "Status" },
  { id: "tools", label: "Tools" },
  { id: "mcp", label: "MCP" },
  { id: "voice", label: "Voice" },
  { id: "conversation", label: "Conversation" },
  { id: "debug", label: "Debug" },
  { id: "settings", label: "Settings" },
] as const;

export type Tab = (typeof TABS)[number]["id"];

export type Screen = "starting" | "onboarding" | Tab;

// What the window can be told. Core events arrive from the bus; `navigate` is the one thing a
// person does that changes state, and it goes through the same reducer so there is exactly one
// place that decides which screen is showing.
//
// The two peer actions carry what a *command* answered rather than what the bus published, and
// they are two rather than one on purpose. `peerQuestion` can only ever name a question, so there
// is no way to write the call that folds in an absence; `peerQuestionEnded` is the only way to
// clear one and it has to name which, because by the time it is dispatched the question it refers
// to may no longer be the one on the screen. See the reducer arms.
export type Action =
  | CoreEvent
  | { kind: "navigate"; to: Tab }
  | { kind: "peerQuestion"; question: PeerQuestion }
  | { kind: "peerQuestionEnded"; id: number }
  | { kind: "resync" };

// How many calls the window keeps and how many it asks the audit file for. This is a tail, not a
// record: the whole history is the file on disk, and an unbounded list would grow for as long as
// the app is left running.
export const MAX_TOOL_CALLS = 50;

export type State = {
  screen: Screen;
  code: { userCode: string; verificationUri: string } | null;
  node: { nodeId: string; nodeName: string } | null;
  connected: boolean;
  problem: string | null;
  // Only meaningful alongside `problem` on the status screen: whether the link is redialling on
  // its own (true) or this is a dead end that needs a restart (false). See `disconnected` below.
  retrying: boolean;
  // Whether the machine is refusing new tool calls. "No new calls" is the whole of it — a call
  // already running and a stream already open both continue, so nothing here may be worded as
  // "nothing is running".
  paused: boolean;
  // Newest first, capped at MAX_TOOL_CALLS. Only the calls this window saw live; the durable
  // tail comes from the audit file through a command.
  toolCalls: ToolCallRow[];
  // The machine waiting to be approved, or null. There is never more than one: the core refuses a
  // second question while one waits rather than queueing it, and keeps refusing for a moment after
  // one ends, so that nobody is trained to click through a fingerprint they did not read.
  //
  // Not a `Screen`. A question arrives while somebody is somewhere — mid-way through the Settings
  // switch, reading the audit tail — and it has to hand that screen back untouched when it is
  // answered, which a screen change cannot do. App.tsx renders it over whatever is showing.
  question: PeerQuestion | null;
  // The last thing the core said about a local MCP server, or null before it has said anything.
  //
  // **A trigger rather than a record.** The MCP screen re-reads the whole list through
  // `mcp_servers` whenever this changes, instead of patching the row the event names: the
  // supervisor is the authority on what is announced, and a screen that applied events to its own
  // copy would be a second opinion that drifts the moment one is dropped. What the event is for is
  // knowing that *something* moved — a server dying is the case that matters, because nobody
  // clicked anything and the row would otherwise go on saying "running" until the window was
  // reopened.
  mcpChange: McpServerEvent | null;
  // How many times this window has been told it fell behind. A counter and nothing else: what it
  // is for is being a value that changes, so an effect watching it runs again.
  //
  // **The half `mcpChange` cannot cover.** A server change is published transiently, so a window
  // that fell behind on the event bus has no way to learn which server moved — the core keeps no
  // last one to hand back. Without this, such a window would go on showing a dead MCP server as
  // running, and the Tools screen would go on listing a capability this computer has withdrawn,
  // until somebody navigated away and back. See `RESYNC_EVENT_NAME` in
  // crates/zyris-app/src/bridge.rs.
  resyncs: number;
};

export const initialState: State = {
  screen: "starting",
  code: null,
  node: null,
  connected: false,
  problem: null,
  retrying: false,
  paused: false,
  toolCalls: [],
  question: null,
  mcpChange: null,
  resyncs: 0,
};

// Where a core event that means "past enrolment" leaves the window.
//
// It must not take a person off a screen they chose. `connecting` and `disconnected` keep
// arriving for as long as the app runs — every reconnect is another pair — and a reconnect that
// threw someone off the Tools tab mid-read would make that tab unusable. So these events only
// claim the screens nobody chose. Idempotent, which is what keeps the replay guarantee below
// true for the arms that use it.
//
// Every screen added to `Tab` passes through here untouched by construction, which is the point
// of naming the two it does claim rather than the ones it does not. Settings is the case that
// would hurt most: a reconnect landing while somebody is halfway through moving the autostart
// switch would take the screen out from under them.
function pastEnrolment(screen: Screen): Screen {
  return screen === "starting" || screen === "onboarding" ? "status" : screen;
}

// Applying the same event twice in a row must leave state exactly as applying it once did — the
// catch-up call in App.tsx can hand the reducer an event already delivered live, and there is no
// way to tell in advance whether it will. Every arm below either fully determines the next state
// from the event alone (so a repeat is a no-op) or falls through untouched (`default`), which is
// what makes that safe.
//
// `toolCall` is the one arm that appends rather than replaces, so a duplicate would show the
// same call twice. It is safe only because the Rust side publishes tool calls through
// `EventBus::publish_transient`, which deliberately leaves them out of the one-slot value
// `fetchLatestEvent` reads — so a `toolCall` can never arrive by that second route. If that ever
// changes, this arm has to change with it.
export function reduce(state: State, action: Action): State {
  switch (action.kind) {
    case "navigate":
      return { ...state, screen: action.to };
    case "needsEnrolment":
      // A fresh enrolment attempt must not carry a code from a previous one.
      return { ...state, screen: "onboarding", code: null, problem: null };
    case "enrolmentCode":
      return {
        ...state,
        screen: "onboarding",
        code: { userCode: action.userCode, verificationUri: action.verificationUri },
        problem: null,
      };
    case "enrolmentFailed":
      // Clear the code: a failure means it is no longer live, and the screen must not show a
      // dead code as though it were still waiting for approval.
      return { ...state, screen: "onboarding", code: null, problem: action.reason };
    case "setupFailed":
      // Reached only before a link ever came up (a stored secret could not be read, or this
      // node could not be registered), so the onboarding screen — not status — is the honest
      // place to show it; Onboarding.tsx explains that a restart is needed.
      return { ...state, screen: "onboarding", code: null, problem: action.reason };
    case "connecting":
      // Leaving the node in place: during a reconnect it is still the same node, and blanking
      // the name would make the screen flicker between identities.
      return { ...state, screen: pastEnrolment(state.screen), connected: false, problem: null };
    case "connected":
      return {
        ...state,
        screen: pastEnrolment(state.screen),
        connected: true,
        code: null,
        node: { nodeId: action.nodeId, nodeName: action.nodeName },
        problem: null,
      };
    case "disconnected":
      return {
        ...state,
        screen: pastEnrolment(state.screen),
        connected: false,
        problem: action.reason,
        retrying: action.retrying,
      };
    case "paused":
      return { ...state, paused: action.paused };
    case "needsPeerApproval":
      // Replaces rather than appends, which is what makes it idempotent: the same question
      // arriving twice — live, and again when the forwarder resends it after falling behind —
      // leaves the same question showing. Only one can ever be waiting, so there is nothing to
      // queue behind it.
      return {
        ...state,
        question: { id: action.id, label: action.label, fingerprint: action.fingerprint },
      };
    case "peerQuestion":
      // What a command said, which is the authority on what is waiting right now. Replaces
      // whatever was showing, for the same reason `needsPeerApproval` does: only one question can
      // be waiting at a time, so there is nothing to queue behind it.
      return { ...state, question: action.question };
    case "peerQuestionEnded":
      // **Only the question it names.** The person answered, or nobody did in time, or they
      // closed a question that had already gone — and every one of those is decided a round trip
      // before this arrives. `Pending::answer` empties the slot before its reply leaves the
      // process, so a second question can be installed and its `needsPeerApproval` can reach this
      // reducer *ahead of* the reply to the first. Clearing unconditionally would blank that
      // second question, unmount PeerConfirm.tsx, stop the poll that would have re-surfaced it,
      // and leave a live question holding the slot invisibly for its whole 45 seconds while
      // somebody sat at the screen.
      //
      // The same guard `Pending::withdraw` applies on the Rust side, and the same one
      // PeerConfirm.tsx's recheck applies when it decides whether to replace what it is showing.
      return state.question?.id === action.id ? { ...state, question: null } : state;
    case "toolCall":
      return {
        ...state,
        toolCalls: [
          {
            // The event carries no time of its own — its wire shape is pinned by a test in
            // event.rs and has no `at` — so this is when the window heard about the call, not
            // when the core wrote it down. The two are a round trip apart, milliseconds, and
            // both clocks are this machine's. Reading one now is the reducer's only impurity;
            // the alternative is a row whose time column is blank while the row beside it,
            // read back from the same call's line in the audit file, has one.
            at: new Date().toISOString(),
            capability: action.capability,
            tool: action.tool,
            detail: action.detail,
            outcome: action.outcome,
          },
          ...state.toolCalls,
        ].slice(0, MAX_TOOL_CALLS),
      };
    case "resync":
      // The one arm that is deliberately **not** idempotent, and it is not reachable by the route
      // that makes idempotence necessary: this is never in the bus's one-slot catch-up value —
      // it is not a core event at all — so it arrives once per time the window actually fell
      // behind. Being told twice means falling behind twice, and each of those is a reason to
      // read again.
      return { ...state, resyncs: state.resyncs + 1 };
    case "mcpServer":
      // Replaces rather than appends, so applying the same event twice leaves the same value —
      // the rule every arm here follows. Two changes that are equal in content do produce two
      // different objects, and that is deliberate: the MCP screen watches this for a change of
      // identity and re-reads the list, so a second death notice for the same server still gets
      // a fresh read rather than being swallowed as a repeat.
      return { ...state, mcpChange: { server: action.server, change: action.change } };
    default:
      return state;
  }
}

// "core-event" here has to match EVENT_NAME in crates/zyris-app/src/bridge.rs exactly; nothing
// checks that at build time, so a rename on either side breaks this silently. The Rust side pins
// its half with a test — see bridge.rs and event.rs — this comment is this side's.
const EVENT_NAME = "core-event";

export function subscribe(onEvent: (event: CoreEvent) => void): Promise<() => void> {
  return listen<CoreEvent>(EVENT_NAME, (message) => onEvent(message.payload));
}

// "You fell behind; ask your questions again." Has to match RESYNC_EVENT_NAME in
// crates/zyris-app/src/bridge.rs exactly; nothing checks that at build time.
//
// A second subscription rather than a `CoreEvent`, because it is not one: every variant of that
// union is something the core did, and this is something this window did. The Rust side says the
// same thing from its own end.
const RESYNC_EVENT_NAME = "core-resync";

export function subscribeResync(onResync: () => void): Promise<() => void> {
  return listen(RESYNC_EVENT_NAME, () => onResync());
}

// What the voice session did. Mirrors `VoiceEvent` in crates/zyris-voice/src/lib.rs, which is
// serialized as a tagged union in camelCase and has a test pinning that shape.
//
// **Not a `CoreEvent` and not in the reducer.** Every variant of that union is something the node
// did about its connection to Attacca; this is a microphone, and nothing outside the Voice screen
// needs to know about it. `heardNothing` is its own variant rather than `heard` with an empty
// string because the two are shown differently — an empty transcript is also what a broken
// microphone produces.
export type VoiceEvent =
  | { kind: "listening" }
  | { kind: "thinking" }
  | { kind: "heard"; text: string }
  | { kind: "heardNothing" }
  | { kind: "failed"; reason: string }
  | { kind: "speaking" }
  | { kind: "spoke" }
  | { kind: "interrupted" };

// Has to match VOICE_EVENT_NAME in crates/zyris-app/src/bridge.rs exactly; nothing checks that
// at build time.
const VOICE_EVENT_NAME = "voice-event";

// Every step the audio took. Mirrors `Trace` in crates/zyris-voice/src/lib.rs, tagged on `step`.
//
// **A second stream rather than more arms on `VoiceEvent`.** That one is the product — four or
// five things a person is told, each of which the Voice screen renders as a state. This is the
// trace: noisy, about the machine rather than the conversation, and read by whoever is asking
// "where did it stop".
export type Trace =
  | { step: "key"; down: boolean }
  | { step: "recording"; started: boolean }
  | { step: "woke"; distance: number; threshold: number }
  | { step: "recorded"; seconds: number; speechSeconds: number; kept: boolean }
  | { step: "transcribing"; seconds: number }
  | { step: "transcribed"; text: string; tookMs: number }
  | { step: "hearing"; text: string; seconds: number }
  | { step: "sent"; text: string }
  | { step: "sendFailed"; reason: string }
  | { step: "delta"; kind: string; text: string }
  | { step: "fragment"; text: string }
  | { step: "synthesised"; text: string; seconds: number; tookMs: number }
  | { step: "queued"; text: string; atSample: number; samples: number }
  | { step: "playing"; atSample: number }
  | { step: "dropped" }
  | { step: "spoke" }
  | { step: "interrupted"; heard: number; unheard: number }
  | { step: "failed"; reason: string };

// Has to match VOICE_TRACE_NAME in crates/zyris-app/src/bridge.rs.
const VOICE_TRACE_NAME = "voice-trace";

// No catch-up here either, and it costs more than it does for `subscribeVoice`: a window opened
// after a turn has no way to see that turn's steps. Holding them on the Rust side would be a
// second copy of the stream with a retention policy, for a screen whose whole job is to watch
// what happens next.
export function subscribeTrace(onStep: (step: Trace) => void): Promise<() => void> {
  return listen<Trace>(VOICE_TRACE_NAME, (message) => onStep(message.payload));
}

// There is deliberately **no catch-up call beside this one**. A turn is four events over a second
// or two and none of them is state, so there is nothing on the Rust side holding the last one to
// hand back. What is state — whether a microphone is open, and why not — comes from `voice_state`,
// which the screen calls on the way in.
export function subscribeVoice(onEvent: (event: VoiceEvent) => void): Promise<() => void> {
  return listen<VoiceEvent>(VOICE_EVENT_NAME, (message) => onEvent(message.payload));
}

// What the core published before this window's listener was registered — see the module comment
// on crates/zyris-app/src/bridge.rs for why that gap exists and is otherwise unrecoverable.
// Meant to be called exactly once, right after `subscribe`'s promise resolves.
export function fetchLatestEvent(): Promise<CoreEvent | null> {
  return invoke<CoreEvent | null>("latest_event");
}

// The question waiting for a person right now, if there is one.
//
// Two callers, for two different reasons. App.tsx asks once at startup, because
// `needsPeerApproval` is published transiently and so is never in the one-slot catch-up value
// `fetchLatestEvent` reads — a window raised by the question itself may not have registered its
// listener by the time the question went past. PeerConfirm.tsx asks repeatedly while it is
// showing one, because a question can stop waiting with no event to say so: its caller can be cut
// off mid-ask, which cancels the future and clears the slot from inside a destructor with nowhere
// to publish from.
export function fetchPendingPeer(): Promise<PeerQuestion | null> {
  return invoke<PeerQuestion | null>("pending_peer");
}
