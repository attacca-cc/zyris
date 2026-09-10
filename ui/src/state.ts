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

export type Screen = "starting" | "onboarding" | "status";

// How many calls the window keeps. This is a tail, not a record: the whole history is the audit
// file on disk, and an unbounded list would grow for as long as the app is left running.
const MAX_TOOL_CALLS = 50;

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
  toolCalls: ToolCall[];
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
export function reduce(state: State, event: CoreEvent): State {
  switch (event.kind) {
    case "needsEnrolment":
      // A fresh enrolment attempt must not carry a code from a previous one.
      return { ...state, screen: "onboarding", code: null, problem: null };
    case "enrolmentCode":
      return {
        ...state,
        screen: "onboarding",
        code: { userCode: event.userCode, verificationUri: event.verificationUri },
        problem: null,
      };
    case "enrolmentFailed":
      // Clear the code: a failure means it is no longer live, and the screen must not show a
      // dead code as though it were still waiting for approval.
      return { ...state, screen: "onboarding", code: null, problem: event.reason };
    case "setupFailed":
      // Reached only before a link ever came up (a stored secret could not be read, or this
      // node could not be registered), so the onboarding screen — not status — is the honest
      // place to show it; Onboarding.tsx explains that a restart is needed.
      return { ...state, screen: "onboarding", code: null, problem: event.reason };
    case "connecting":
      // Leaving the node in place: during a reconnect it is still the same node, and blanking
      // the name would make the screen flicker between identities.
      return { ...state, screen: "status", connected: false, problem: null };
    case "connected":
      return {
        ...state,
        screen: "status",
        connected: true,
        code: null,
        node: { nodeId: event.nodeId, nodeName: event.nodeName },
        problem: null,
      };
    case "disconnected":
      return {
        ...state,
        screen: "status",
        connected: false,
        problem: event.reason,
        retrying: event.retrying,
      };
    case "paused":
      return { ...state, paused: event.paused };
    case "toolCall":
      return {
        ...state,
        toolCalls: [
          { capability: event.capability, tool: event.tool, detail: event.detail, outcome: event.outcome },
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
