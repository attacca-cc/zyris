import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn<(command: string, args?: unknown) => Promise<unknown>>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/app", () => ({ getVersion: () => Promise.resolve("0.5.0") }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ onFocusChanged: () => Promise.resolve(() => {}) }),
}));

import { Settings } from "./Settings";
import { initialState } from "./state";

describe("Settings", () => {
  beforeEach(() => {
    invoke.mockImplementation(() => Promise.resolve(null));
  });
  afterEach(() => {
    cleanup();
    invoke.mockReset();
  });

  it("shows the connection that used to be its own screen", () => {
    render(<Settings state={{ ...initialState, screen: "settings", connected: true, node: { nodeId: "n1", nodeName: "desk-linux" } }} />);
    expect(screen.getByText("Connected")).toBeTruthy();
    expect(screen.getByText("desk-linux")).toBeTruthy();
  });

  it("forgets the credential only on the second press", async () => {
    render(<Settings state={{ ...initialState, screen: "settings" }} />);

    fireEvent.click(screen.getByRole("button", { name: "Disconnect" }));
    expect(invoke).not.toHaveBeenCalledWith("forget_credential");

    fireEvent.click(screen.getAllByRole("button", { name: "Disconnect" }).at(-1)!);
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("forget_credential"));
  });
});
