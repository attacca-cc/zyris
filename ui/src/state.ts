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
  | ({ kind: "toolCall" } & ToolCall);

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

// The two screens a person can move between once this machine is enrolled. `starting` and
// `onboarding` are not among them: they are where the core puts the window, not where anyone
// chooses to be.
export type Tab = "status" | "tools";

export type Screen = "starting" | "onboarding" | Tab;

// What the window can be told. Core events arrive from the bus; `navigate` is the one thing a
// person does that changes state, and it goes through the same reducer so there is exactly one
// place that decides which screen is showing.
export type Action = CoreEvent | { kind: "navigate"; to: Tab };

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
};

// Where a core event that means "past enrolment" leaves the window.
//
// It must not take a person off a screen they chose. `connecting` and `disconnected` keep
// arriving for as long as the app runs — every reconnect is another pair — and a reconnect that
// threw someone off the Tools tab mid-read would make that tab unusable. So these events only
// claim the screens nobody chose. Idempotent, which is what keeps the replay guarantee below
// true for the arms that use it.
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
