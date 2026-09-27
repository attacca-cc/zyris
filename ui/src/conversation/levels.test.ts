import { describe, expect, it } from "vitest";
import { Envelope, perceived } from "./levels";

describe("perceived", () => {
  it("maps silence to nothing and a loud voice to all of it", () => {
    expect(perceived(0)).toBe(0);
    expect(perceived(Number.NaN)).toBe(0);
    expect(perceived(10 ** (-50 / 20))).toBeCloseTo(0);
    expect(perceived(10 ** (-30 / 20))).toBeCloseTo(0.5);
    expect(perceived(1)).toBe(1);
  });
});

describe("Envelope", () => {
  it("rises faster than it falls", () => {
    const rising = new Envelope();
    rising.set(1);
    const up = rising.step(0.05);
    const falling = new Envelope();
    falling.value = 1;
    falling.set(0);
    const down = 1 - falling.step(0.05);
    expect(up).toBeGreaterThan(down);
  });

  it("settles on its target", () => {
    const envelope = new Envelope();
    envelope.set(0.6);
    for (let i = 0; i < 100; i += 1) envelope.step(0.05);
    expect(envelope.value).toBeCloseTo(0.6);
  });
});
