import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Both module boundaries this screen reaches through, mocked. Through `vi.hoisted` because
// `vi.mock`'s factory is lifted above the imports, and a factory closing over an ordinary `const`
// reaches for a binding that has not been evaluated yet.
const { invoke, onFocusChanged } = vi.hoisted(() => ({
  invoke: vi.fn<(command: string, args?: unknown) => Promise<unknown>>(),
  onFocusChanged: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ onFocusChanged }) }));

import { Tools } from "./Tools";
import { initialState, type McpServerEvent, type State } from "./state";

type Announced = { name: string; version: number; tools: string[] };

function capability(name: string, tools: string[] = ["search"]): Announced {
  return { name, version: 1, tools };
}

// What every command this screen calls answers with, with `announced_tools` saying whatever the
// test wants this computer to be announcing at the moment it is asked.
function announcing(capabilities: Announced[]) {
  invoke.mockImplementation((command: string) => {
    switch (command) {
      case "announced_tools":
        return Promise.resolve({
          capabilities,
          root: "/home/ada",
          auditLog: "/home/ada/.local/share/zyris/audit.jsonl",
        });
      case "recent_tool_calls":
        return Promise.resolve([]);
      case "inbox":
        return Promise.resolve(null);
      case "is_paused":
        return Promise.resolve(false);
      default:
        return Promise.resolve(null);
    }
  });
}

function showing(mcpChange: McpServerEvent | null = null): State {
  return { ...initialState, screen: "tools", mcpChange };
}

function readable(): string {
  return document.body.textContent ?? "";
}

describe("Tools", () => {
  beforeEach(() => {
    invoke.mockReset();
    onFocusChanged.mockClear();
    announcing([capability("terminal", ["exec"])]);
  });

  afterEach(cleanup);

  it("asks again when the core says a local MCP server moved", async () => {
    // **The defect this test exists for.** What this computer announces stopped being fixed for
    // the run the moment a promoted MCP server could be turned off, turned on, or die — and a
    // screen that read the list once went on telling a person their agents could reach a
    // capability the node had withdrawn. `mcpChange` is the only thing that can change this
    // answer, and asking again is the whole fix on this side.
    announcing([capability("terminal", ["exec"]), capability("mcp_desk-notes")]);
    const { rerender } = render(<Tools state={showing()} dispatch={() => {}} />);
    await waitFor(() => expect(readable()).toContain("mcp_desk-notes"));

    announcing([capability("terminal", ["exec"])]);
    rerender(
      <Tools
        state={showing({ server: "desk-notes", change: { change: "disabled" } })}
        dispatch={() => {}}
      />,
    );

    await waitFor(() =>
      expect(readable()).not.toContain("mcp_desk-notes"),
    );
    expect(readable()).toContain("terminal");
  });

  it("keeps showing what it last heard when a later read fails", async () => {
    // A read that did not come back is not a computer that offers nothing. The list on the screen
    // is still the last thing this computer said about itself.
    announcing([capability("terminal", ["exec"])]);
    const { rerender } = render(<Tools state={showing()} dispatch={() => {}} />);
    await waitFor(() => expect(readable()).toContain("terminal"));

    invoke.mockImplementation((command: string) =>
      command === "announced_tools"
        ? Promise.reject("the bridge is gone")
        : Promise.resolve(command === "recent_tool_calls" ? [] : null),
    );
    rerender(
      <Tools
        state={showing({ server: "desk-notes", change: { change: "died" } })}
        dispatch={() => {}}
      />,
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledWith("announced_tools"));
    expect(readable()).toContain("terminal");
  });

  it("says a read failed rather than that this computer offers nothing", async () => {
    invoke.mockImplementation((command: string) =>
      command === "announced_tools"
        ? Promise.reject("the bridge is gone")
        : Promise.resolve(command === "recent_tool_calls" ? [] : null),
    );

    render(<Tools state={showing()} dispatch={() => {}} />);

    await waitFor(() => expect(screen.getByText("the bridge is gone")).toBeTruthy());
  });
});
