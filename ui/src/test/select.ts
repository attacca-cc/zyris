import { fireEvent, screen, within } from "@testing-library/react";

// Driving a Radix Select the way a keyboard does. jsdom has no layout, so pointer-driven opening
// is not worth imitating; the keyboard path is the same component code a person reaches.

export function openSelect(name: RegExp | string): HTMLElement {
  const trigger = screen.getByRole("combobox", { name });
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  return screen.getByRole("listbox");
}

// The options a select offers, in order, by their text.
export function optionsOf(name: RegExp | string): string[] {
  const listbox = openSelect(name);
  const names = within(listbox)
    .getAllByRole("option")
    .map((option) => option.textContent ?? "");
  fireEvent.keyDown(listbox, { key: "Escape" });
  return names;
}

// Pick the option whose text matches.
export function choose(name: RegExp | string, option: RegExp | string) {
  const listbox = openSelect(name);
  fireEvent.click(within(listbox).getByRole("option", { name: option }));
}

// The text a select shows as chosen.
export function shown(name: RegExp | string): string {
  return screen.getByRole("combobox", { name }).textContent ?? "";
}
