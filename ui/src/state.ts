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

export function subscribe(onEvent: (event: CoreEvent) => void): Promise<() => void> {
  return listen<CoreEvent>("core-event", (message) => onEvent(message.payload));
}
