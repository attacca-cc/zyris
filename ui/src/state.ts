import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Mirrors CoreEvent in crates/zyris-core/src/event.rs. Serialized there as a tagged union with
// camelCase fields, so this is a transcription rather than a parse.
export type CoreEvent =
  | { kind: "started" }
  | { kind: "shuttingDown" }
  | { kind: "needsEnrolment" }
  | { kind: "enrolmentCode"; userCode: string; verificationUri: string }
  | { kind: "enrolmentFailed"; reason: string }
  | { kind: "connecting" }
  | { kind: "connected"; nodeId: string; nodeName: string }
  | { kind: "disconnected"; reason: string };

export type Screen = "starting" | "onboarding" | "status";

export type State = {
  screen: Screen;
  code: { userCode: string; verificationUri: string } | null;
  node: { nodeId: string; nodeName: string } | null;
  connected: boolean;
  problem: string | null;
};

export const initialState: State = {
  screen: "starting",
  code: null,
  node: null,
  connected: false,
  problem: null,
};

// Applying the same event twice in a row must leave state exactly as applying it once did — the
// catch-up call in App.tsx can hand the reducer an event already delivered live, and there is no
// way to tell in advance whether it will. Every arm below either fully determines the next state
// from the event alone (so a repeat is a no-op) or falls through untouched (`default`), which is
// what makes that safe.
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
      return { ...state, screen: "status", connected: false, problem: event.reason };
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
