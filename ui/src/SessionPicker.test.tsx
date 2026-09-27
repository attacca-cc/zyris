import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { SessionPicker, type SessionsView } from "./SessionPicker";
import { choose, optionsOf, shown } from "./test/select";

const ACCOUNT: SessionsView = {
  projects: [
    { id: "p-home", name: "Home", isDefault: true },
    { id: "p-work", name: "Work", isDefault: false },
  ],
  sessions: [
    { id: "s-lunch", title: "Lunch plans", project: "p-home", agent: "a1", running: false },
    { id: "s-report", title: "Quarterly report", project: "p-work", agent: "a1", running: true },
    // No project: filed under the default one.
    { id: "s-loose", title: null, project: null, agent: "a1", running: false },
  ],
  agents: [{ id: "a1", name: "Ada" }],
  current: "s-report",
  problems: [],
};

const options = optionsOf;

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("the session picker", () => {
  it("opens on the project the current session is in, and lists only that project's sessions", async () => {
    invoke.mockResolvedValue(ACCOUNT);
    render(<SessionPicker hidden={false} onSwitched={() => {}} />);

    await screen.findByRole("combobox", { name: /^project$/i });
    expect(shown(/^project$/i)).toBe("Work");
    expect(shown(/^session$/i)).toBe("Quarterly report — answering");
    expect(options(/^session$/i)).toEqual(["Quarterly report — answering"]);

    choose(/^project$/i, /^Home/);
    // The one in use stays visible as the choice, marked as elsewhere, rather than the first
    // session of Home looking chosen.
    expect(options(/^session$/i)).toEqual([
      "Quarterly report (another project)",
      "Lunch plans",
      "Untitled session (s-loose)",
    ]);
  });

  it("switches session through the Rust side and says the conversation changed", async () => {
    invoke.mockResolvedValueOnce(ACCOUNT).mockResolvedValueOnce({ ...ACCOUNT, current: "s-lunch" });
    const onSwitched = vi.fn();
    render(<SessionPicker hidden={false} onSwitched={onSwitched} />);

    await screen.findByRole("combobox", { name: /^project$/i });
    choose(/^project$/i, /^Home/);
    choose(/^session$/i, "Lunch plans");

    await waitFor(() => expect(onSwitched).toHaveBeenCalledTimes(1));
    expect(invoke).toHaveBeenLastCalledWith("choose_conversation_session", { session: "s-lunch" });
  });

  it("starts a new session in the project on screen, leaving the only agent to the Rust side", async () => {
    invoke
      .mockResolvedValueOnce(ACCOUNT)
      .mockResolvedValueOnce({ ...ACCOUNT, current: "s-new" });
    const onSwitched = vi.fn();
    render(<SessionPicker hidden={false} onSwitched={onSwitched} />);

    await screen.findByRole("combobox", { name: /^project$/i });
    // One agent is not a choice, so there is nothing to pick it from.
    expect(screen.queryByRole("combobox", { name: /agent/i })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /new session/i }));

    await waitFor(() => expect(onSwitched).toHaveBeenCalledTimes(1));
    expect(invoke).toHaveBeenLastCalledWith("new_conversation_session", {
      project: "p-work",
      agent: null,
    });
  });

  it("asks which agent when the account has several, and sends that one", async () => {
    const several = {
      ...ACCOUNT,
      agents: [
        { id: "a1", name: "Ada" },
        { id: "a2", name: "Bea" },
      ],
    };
    invoke.mockResolvedValueOnce(several).mockResolvedValueOnce({ ...several, current: "s-new" });
    render(<SessionPicker hidden={false} onSwitched={() => {}} />);

    await screen.findByRole("combobox", { name: /agent for a new session/i });
    choose(/agent for a new session/i, "Bea");
    fireEvent.click(screen.getByRole("button", { name: /new session/i }));

    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("new_conversation_session", {
        project: "p-work",
        agent: "a2",
      }),
    );
  });

  it("still offers every session when the projects cannot be read", async () => {
    // What a real machine enrolled without `projects:read` got: nothing but the error.
    invoke
      .mockResolvedValueOnce({
        ...ACCOUNT,
        projects: [],
        problems: ["projects: this credential was not granted the projects:read scope"],
      })
      .mockResolvedValueOnce({ ...ACCOUNT, projects: [], problems: [], current: "s-new" });
    render(<SessionPicker hidden={false} onSwitched={() => {}} />);

    await screen.findByRole("combobox", { name: /^session$/i });
    expect(screen.queryByRole("combobox", { name: /^project$/i })).toBeNull();
    expect(options(/^session$/i)).toEqual([
      "Lunch plans",
      "Quarterly report — answering",
      "Untitled session (s-loose)",
    ]);
    expect(document.body.textContent).toMatch(/projects:read/);

    fireEvent.click(screen.getByRole("button", { name: /new session/i }));
    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith("new_conversation_session", {
        project: null,
        agent: null,
      }),
    );
  });

  it("says why there is nothing to choose from rather than showing an empty list", async () => {
    invoke.mockRejectedValue("this machine is not connected to Attacca yet");
    render(<SessionPicker hidden={false} onSwitched={() => {}} />);

    expect(await screen.findByText(/not connected to Attacca yet/)).toBeTruthy();
    expect(screen.queryByRole("combobox")).toBeNull();
  });
});
