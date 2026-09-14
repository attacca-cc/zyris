import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The whole of what this screen can do to the machine, mocked at the module boundary. Through
// `vi.hoisted` because `vi.mock`'s factory is lifted above the imports, and a factory closing over
// an ordinary `const` reaches for a binding that has not been evaluated yet.
const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn<(command: string, args?: unknown) => Promise<unknown>>(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { Mcp } from "./Mcp";
import { initialState, type McpServerEvent, type State } from "./state";

const PATH = "/home/ada/.local/share/zyris/mcp-servers.json";

type ServerView = {
  name: string;
  capability: string | null;
  command: string;
  args: string[];
  state:
    | { state: "running" }
    | { state: "disabled" }
    | { state: "died" }
    | { state: "failed"; reason: string };
  tools: string[];
  dropped: { name: string; reason: string }[];
};

function server(name: string, over: Partial<ServerView> = {}): ServerView {
  return {
    name,
    capability: `mcp_${name}`,
    command: "notes-mcp",
    args: ["--root", "/home/ada/notes"],
    state: { state: "running" },
    tools: ["search", "append"],
    dropped: [],
    ...over,
  };
}

// The answer `mcp_servers` gives, with whatever this test wants to be true of it.
function answers(list: { problem?: string | null; servers?: ServerView[] }) {
  invoke.mockImplementation((command: string) => {
    if (command === "mcp_servers") {
      return Promise.resolve({
        path: PATH,
        problem: list.problem ?? null,
        servers: list.servers ?? [],
      });
    }
    return Promise.resolve(null);
  });
}

function showing(mcpChange: McpServerEvent | null = null, resyncs = 0): State {
  return { ...initialState, screen: "mcp", mcpChange, resyncs };
}

// Everything a person can actually read on the screen, as one string. Deliberately the rendered
// text and not the DOM: a test that asserted on class names would pass for a screen that painted
// two different states the same colour and said the same words about both.
function readable(): string {
  return document.body.textContent ?? "";
}

describe("Mcp", () => {
  beforeEach(() => {
    invoke.mockReset();
    answers({ servers: [] });
  });

  afterEach(cleanup);

  it("does not say there are no servers while the list is still being read", async () => {
    // A promise that never settles: the read is in flight, which is a third answer and not an
    // empty list.
    invoke.mockImplementation(() => new Promise(() => {}));

    render(<Mcp state={showing()} />);

    expect(readable()).not.toMatch(/no MCP servers/i);
    // And it says what it is doing, rather than sitting blank — a screen with nothing on it also
    // passes the assertion above.
    expect(readable()).toMatch(/reading the server list/i);
  });

  it("says a list that could not be read could not be read, and never that there are none", async () => {
    // **The failure this screen exists not to repeat.** A file with a typo in it starts no
    // servers, exactly as a machine nobody has configured does, and telling the person who wrote
    // that file they have configured nothing sends them looking for the file they are looking at.
    answers({ problem: "expected value at line 1 column 1", servers: [] });

    render(<Mcp state={showing()} />);

    await screen.findByText(/expected value at line 1 column 1/);
    expect(readable()).not.toMatch(/no MCP servers/i);
  });

  it("says there are none only when the list was read and is empty", async () => {
    answers({ servers: [] });

    render(<Mcp state={showing()} />);

    await waitFor(() => expect(readable()).toMatch(/no MCP servers/i));
    // And it names the file, because "add one" is not an instruction without it.
    expect(screen.getAllByText(PATH).length).toBeGreaterThan(0);
    // Nor is it one without the shape of an entry. This is the only screen where somebody with no
    // servers ever is, and the file is the only way to get one; a sentence about a file with no
    // example of what goes in it sends them to the README, which they may not have.
    expect(readable()).toContain('"servers"');
    expect(readable()).toContain('"command"');
  });

  it("says a read that failed outright failed, rather than showing an empty list", async () => {
    invoke.mockImplementation(() => Promise.reject("the bridge is not there"));

    render(<Mcp state={showing()} />);

    await screen.findByText(/the bridge is not there/);
    expect(readable()).not.toMatch(/no MCP servers/i);
  });

  it("tells a reader a server that died apart from one that was turned off", async () => {
    // Both are "not announced" and an agent cannot tell them apart. A person has to: one is a
    // switch they moved and the other is a process to restart. The assertion is on words, not on
    // a class name, because a class name is not something anybody reads.
    answers({
      servers: [
        server("fell-over", { state: { state: "died" }, tools: [] }),
        server("switched-off", { state: { state: "disabled" }, tools: [] }),
      ],
    });

    render(<Mcp state={showing()} />);

    const died = await screen.findByRole("listitem", { name: "fell-over" });
    const disabled = await screen.findByRole("listitem", { name: "switched-off" });

    expect(died.textContent).not.toEqual(disabled.textContent);
    expect(died.textContent).toMatch(/stopped on its own/i);
    expect(disabled.textContent).toMatch(/turned off/i);
    // And the one nobody asked for says so, since that is the whole of what makes it different.
    expect(died.textContent).toMatch(/nobody asked/i);

    // The word the eye lands on first differs too. Located by class and asserted on its text: a
    // badge that read "turned off" over a paragraph saying the process fell over would be a row
    // contradicting itself, and the badge is the half most people read.
    expect(died.querySelector(".badge")?.textContent).not.toEqual(
      disabled.querySelector(".badge")?.textContent,
    );
  });

  it("names the tools a running server promoted, and how many", async () => {
    answers({ servers: [server("desk-notes", { tools: ["search", "append", "tag"] })] });

    render(<Mcp state={showing()} />);

    const row = await screen.findByRole("listitem", { name: "desk-notes" });
    expect(row.textContent).toMatch(/3 tools/);
    expect(row.textContent).toContain("search, append, tag");
    expect(row.textContent).toContain("mcp_desk-notes");
  });

  it("shows the tools a server offered that this machine did not announce, with the reason", async () => {
    // An agent cannot tell a tool that was dropped from one the server never had. The person can,
    // and this is the only place they could.
    answers({
      servers: [
        server("desk-notes", {
          tools: ["search"],
          dropped: [{ name: "search", reason: "this server offers two tools called `search`" }],
        }),
      ],
    });

    render(<Mcp state={showing()} />);

    const row = await screen.findByRole("listitem", { name: "desk-notes" });
    expect(row.textContent).toMatch(/two tools called `search`/);
  });

  it("tells someone whose server name cannot be announced what to do about it", async () => {
    // `capability` is null for a name no call could be addressed to. The fix is a rename, and the
    // switch is not one: starting it would fail for the same reason every time.
    answers({
      servers: [
        server("my.notes", {
          capability: null,
          state: { state: "failed", reason: "a capability name may not contain a dot" },
          tools: [],
        }),
      ],
    });

    render(<Mcp state={showing()} />);

    const row = await screen.findByRole("listitem", { name: "my.notes" });
    expect(row.textContent).toMatch(/rename/i);
    expect(row.querySelector("button")).toBe(null);
  });

  it("shows a failed server's reason once and does not say it did not start twice", async () => {
    // The row used to be rendered behind a fixed "It did not start.", and the reason the core
    // stamps for a startup failure began the same way — so the sentence repeated its own first
    // clause. The badge already carries those words; the paragraph carries the reason.
    answers({
      servers: [
        server("desk-notes", {
          state: {
            state: "failed",
            reason: "The reason was written to the log when Zyris started.",
          },
          tools: [],
        }),
      ],
    });

    render(<Mcp state={showing()} />);

    const row = await screen.findByRole("listitem", { name: "desk-notes" });
    expect(row.textContent).toContain("did not start");
    expect(row.textContent?.match(/did not start/g)).toHaveLength(1);
    expect(row.textContent).toContain("The reason was written to the log when Zyris started.");
  });

  it("renders what the switch answered rather than what it was asked for", async () => {
    // Turning a server on is the case that proves it: the command may not be there any more, and
    // the honest answer is the failure rather than the "running" the click asked for.
    answers({ servers: [server("desk-notes", { state: { state: "disabled" }, tools: [] })] });
    render(<Mcp state={showing()} />);

    const button = await screen.findByRole("button", { name: /turn on/i });
    invoke.mockImplementation((command: string) => {
      if (command === "set_mcp_server_enabled") {
        return Promise.resolve(
          server("desk-notes", {
            state: { state: "failed", reason: "no such file or directory" },
            tools: [],
          }),
        );
      }
      return Promise.resolve({ path: PATH, problem: null, servers: [] });
    });
    button.click();

    await screen.findByText(/no such file or directory/);
    expect(invoke).toHaveBeenCalledWith("set_mcp_server_enabled", {
      name: "desk-notes",
      enabled: true,
    });
    const row = screen.getByRole("listitem", { name: "desk-notes" });
    expect(row.textContent).not.toMatch(/running/i);
  });

  it("turns a running server off rather than asking for it again", async () => {
    // The switch reads the state it is on, and both directions go through one command. Sending
    // `enabled: true` for a server that is already running is not an error and does nothing — so a
    // switch stuck on "on" would look like a button that does not work, with nothing said about
    // it anywhere.
    answers({ servers: [server("desk-notes")] });
    render(<Mcp state={showing()} />);

    const button = await screen.findByRole("button", { name: /turn off/i });
    button.click();

    expect(invoke).toHaveBeenCalledWith("set_mcp_server_enabled", {
      name: "desk-notes",
      enabled: false,
    });
  });

  it("keeps a switch that was refused from reading as a server that is gone", async () => {
    // A rejected `set_mcp_server_enabled` says why and leaves the row it was about on the screen:
    // the server still exists, and a row that vanished would say it did not.
    answers({ servers: [server("desk-notes", { state: { state: "disabled" }, tools: [] })] });
    render(<Mcp state={showing()} />);

    const button = await screen.findByRole("button", { name: /turn on/i });
    invoke.mockImplementation((command: string) => {
      if (command === "set_mcp_server_enabled") return Promise.reject("it would not start");
      return Promise.resolve({ path: PATH, problem: null, servers: [] });
    });
    button.click();

    await screen.findByText(/it would not start/);
    expect(screen.getByRole("listitem", { name: "desk-notes" })).toBeTruthy();
  });

  it("re-reads the list when the core says a server changed", async () => {
    // The case nobody clicked: a server's process falls over, the core withdraws it and publishes
    // the change. Without this the row goes on saying "running" until the window is reopened —
    // which is exactly the screen the core does all that work to avoid.
    answers({ servers: [server("desk-notes")] });
    const { rerender } = render(<Mcp state={showing()} />);
    await screen.findByRole("listitem", { name: "desk-notes" });

    answers({ servers: [server("desk-notes", { state: { state: "died" }, tools: [] })] });
    rerender(<Mcp state={showing({ server: "desk-notes", change: { change: "died" } })} />);

    await waitFor(() =>
      expect(screen.getByRole("listitem", { name: "desk-notes" }).textContent).toMatch(
        /stopped on its own/i,
      ),
    );
  });

  it("re-reads the list when this window is told it fell behind", async () => {
    // The half `mcpChange` cannot cover, and the one a reviewer found. The forwarder drops a
    // contiguous range of events when the window falls behind on the bus; a server change is
    // published transiently and nothing keeps the last one, so there is no way to say *which*
    // server moved. Without a read on the resync alone, such a window goes on showing a dead
    // server as running with nothing left to correct it.
    answers({ servers: [server("desk-notes")] });
    const { rerender } = render(<Mcp state={showing()} />);
    await screen.findByRole("listitem", { name: "desk-notes" });

    answers({ servers: [server("desk-notes", { state: { state: "died" }, tools: [] })] });
    rerender(<Mcp state={showing(null, 1)} />);

    await waitFor(() =>
      expect(screen.getByRole("listitem", { name: "desk-notes" }).textContent).toMatch(
        /stopped on its own/i,
      ),
    );
  });

  it("says what the audit log does and does not keep about a promoted tool", async () => {
    // The claim this project has got wrong four times. An MCP tool's arguments are deliberately
    // not written down — `LOGGED_FIELDS` matches on spelling, and a third party's `path` could as
    // easily be a password — so the screen has to say so rather than let a reader assume the log
    // records what was asked.
    answers({ servers: [server("desk-notes")] });

    render(<Mcp state={showing()} />);
    await screen.findByRole("listitem", { name: "desk-notes" });

    expect(readable()).toMatch(/not what was asked/i);
  });

  it("does not claim a line for a call that never finished", async () => {
    // The fifth time this copy claimed more than the code does, and the first time a test
    // guarded it. `Guarded::dispatch` writes its line when the call returns; a call cut off
    // before it returns is carried by a task `zyris-core` aborts, and nothing is written. The
    // screen has to say that rather than let a reader take "the log records that an MCP tool was
    // called" as covering every call that started.
    answers({ servers: [server("desk-notes")] });

    render(<Mcp state={showing()} />);
    await screen.findByRole("listitem", { name: "desk-notes" });

    expect(readable()).toMatch(/when a call finishes/i);
    expect(readable()).toMatch(/call that never finishes is not written down/i);
    // And why it can happen at all: nothing on this side stops a server waiting forever.
    expect(readable()).toMatch(/no time limit/i);
  });

  it("says how long a switch moved here lasts", async () => {
    // Nothing is written to the server list, so a server turned off here is back at the next
    // start. A switch that forgot silently would be a screen that lied.
    answers({ servers: [server("desk-notes")] });

    render(<Mcp state={showing()} />);
    await screen.findByRole("listitem", { name: "desk-notes" });

    expect(readable()).toMatch(/until Zyris restarts/i);
    expect(readable()).toMatch(/"enabled": false/);
  });
});
