// How loud, as a size on screen.
//
// The level arrives as linear RMS, which is almost all near zero for speech: a person talking at
// a normal distance from a laptop microphone is 0.01 to 0.1. Decibels spread that out the way an
// ear does, and -50 dB to -10 dB is the range a voice actually moves through.
export function perceived(rms: number): number {
  if (!(rms > 0)) return 0;
  const db = 20 * Math.log10(rms);
  return Math.min(1, Math.max(0, (db + 50) / 40));
}

// A level that moves smoothly between measurements: it rises quickly and falls slowly, the way a
// meter's needle does, so that a gap between words is a dip rather than a collapse.
export class Envelope {
  value = 0;
  private target = 0;

  set(level: number) {
    this.target = level;
  }

  // Advance by `seconds` and answer the value.
  step(seconds: number): number {
    const rate = this.target > this.value ? 18 : 5;
    const k = 1 - Math.exp(-rate * seconds);
    this.value += (this.target - this.value) * k;
    return this.value;
  }
}
