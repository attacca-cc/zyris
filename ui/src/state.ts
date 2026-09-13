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
  | ({ kind: "toolCall" } & ToolCall);

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

// The three screens a person can move between once this machine is enrolled. `starting` and
// `onboarding` are not among them: they are where the core puts the window, not where anyone
// chooses to be.
export type Tab = "status" | "tools" | "settings";

export type Screen = "starting" | "onboarding" | Tab;

// What the window can be told. Core events arrive from the bus; `navigate` is the one thing a
// person does that changes state, and it goes through the same reducer so there is exactly one
// place that decides which screen is showing. `peerQuestion` carries what a command answered
// rather than what the bus published — the question a late window fetched, or the news that the
// one it was showing has stopped waiting.
export type Action =
  | CoreEvent
  | { kind: "navigate"; to: Tab }
  | { kind: "peerQuestion"; question: PeerQuestion | null };

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
  // The machine waiting to be approved, or null. There is never more than one: the core refuses
  // a second question while one waits rather than queueing it, so that nobody is trained to click
  // through a fingerprint they did not read.
  //
  // Not a `Screen`. A question arrives while somebody is somewhere — mid-way through the Settings
  // switch, reading the audit tail — and it has to hand that screen back untouched when it is
  // answered, which a screen change cannot do. App.tsx renders it over whatever is showing.
  question: PeerQuestion | null;
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
      // What a command said, which is the authority on whether a question is still waiting.
      // `null` ends the screen: the person answered, or nobody did in time, or the machine that
      // asked gave up. PeerConfirm.tsx only sends that once it knows which.
      return { ...state, question: action.question };
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
