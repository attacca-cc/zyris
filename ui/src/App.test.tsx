import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Both Tauri boundaries the app reaches through on startup. `listen` is the one under test here:
// what matters is which channels this window actually registers for, which nothing else checks.
const { invoke, listen } = vi.hoisted(() => ({
  invoke: vi.fn<(command: string, args?: unknown) => Promise<unknown>>(),
  listen: vi.fn<(name: string, handler: (m: unknown) => void) => Promise<() => void>>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ onFocusChanged: () => Promise.resolve(() => {}) }),
}));

import { App } from "./App";

describe("App", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(() => Promise.resolve(null));
    listen.mockReset();
    listen.mockImplementation(() => Promise.resolve(() => {}));
  });

  afterEach(cleanup);

  it("listens on both channels the bridge emits on", async () => {
    // **Two names, because one of them is not something the core did.** Core events carry a
    // `CoreEvent`; the resync notice carries nothing and means "you fell behind, ask your
    // questions again" — the only way a window that missed an MCP server change can learn it
    // missed one, since those are published transiently and nothing keeps the last. A window
    // that registers for the first and not the second goes on showing a dead server as running.
    render(<App />);

    await waitFor(() => expect(listen).toHaveBeenCalledTimes(2));
    const names = listen.mock.calls.map(([name]) => name).sort();
    expect(names).toEqual(["core-event", "core-resync"]);
  });
});
