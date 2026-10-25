import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";

const { invoke } = vi.hoisted(() => ({
  invoke: vi.fn<(command: string, args?: unknown) => Promise<unknown>>(() => Promise.resolve()),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { Onboarding } from "./Onboarding";
import { initialState } from "./state";

it("opens the approval page with the code already in the link", () => {
  const code = { userCode: "WXQR-7KBD", verificationUri: "https://attacca.cc/settings/zyris/device" };
  render(<Onboarding state={{ ...initialState, code }} />);

  fireEvent.click(screen.getByRole("button", { name: /Open attacca\.cc/ }));

  expect(invoke).toHaveBeenCalledWith("open_verification_url", {
    url: "https://attacca.cc/settings/zyris/device?code=WXQR-7KBD",
  });
});
