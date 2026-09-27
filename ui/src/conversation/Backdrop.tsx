import { useEffect, useRef } from "react";
import { Envelope } from "./levels";

// What the backdrop is showing.
//
// - `idle`: nobody is talking. A dim sphere of dots, turning slowly.
// - `listening`: the key is down or a turn is being recorded. The sphere brightens and ripples
//   with the microphone.
// - `speaking`: the agent's answer is being played. The sphere gives way to four bars that follow
//   the speaker.
export type Mode = "idle" | "listening" | "speaking";

// A lattice of points spread evenly over a sphere.
function fibonacciSphere(count: number) {
  const points: { x: number; y: number; z: number; phase: number }[] = [];
  const golden = Math.PI * (3 - Math.sqrt(5));
  for (let i = 0; i < count; i += 1) {
    const y = 1 - (i / (count - 1)) * 2;
    const r = Math.sqrt(1 - y * y);
    const theta = golden * i;
    points.push({ x: Math.cos(theta) * r, y, z: Math.sin(theta) * r, phase: (i * 2.39) % (Math.PI * 2) });
  }
  return points;
}

const POINTS = fibonacciSphere(520);
const BAR_WEIGHTS = [0.7, 1, 0.88, 0.62];

// Faint on purpose: it sits behind the text a person is reading.
const DOT_STRENGTH = { idle: 0.09, active: 0.22 };
const BAR_ALPHA = 0.06;

// Drawn behind the conversation, reacting to `level` — a ref, so that twenty-five updates a
// second do not re-render anything. One animation loop at about 30 frames a second; it stops
// while the window or the Conversation screen is hidden, and under reduced motion the picture is
// drawn once per change of mode and left still.
//
// **Cheap per frame on purpose.** WebKitGTK paints a canvas in software, and a fill per dot — five
// hundred a frame — held the main thread long enough that every dropdown in the window opened
// late. The dots are drawn in a few shades, one path and one fill per shade.
const SHADES = 8;
const FRAME_MS = 1000 / 30;

export function Backdrop({
  mode,
  level,
  paused = false,
}: {
  mode: Mode;
  // The Conversation screen is not showing: nothing to draw.
  paused?: boolean;
  // The latest perceived level, 0..1, for whichever source the mode is about.
  level: React.RefObject<number>;
}) {
  const canvas = useRef<HTMLCanvasElement | null>(null);
  const modeRef = useRef<Mode>(mode);
  modeRef.current = mode;
  // Draws one frame now; set by the loop below. Under reduced motion there is no loop, so a
  // change of mode calls this to draw the new picture.
  const redraw = useRef<(() => void) | null>(null);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const wake = useRef<(() => void) | null>(null);

  useEffect(() => {
    const element = canvas.current;
    const context = element?.getContext?.("2d");
    if (!element || !context) return;

    const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    const envelope = new Envelope();
    const bars = [0, 0, 0, 0];
    let mix = modeRef.current === "speaking" ? 1 : 0;
    let active = modeRef.current === "listening" ? 1 : 0;
    let frame = 0;
    let last = performance.now();
    const started = last;
    const shades: { x: number; y: number; r: number }[][] = Array.from({ length: SHADES }, () => []);
    const running = () => !reduced && !document.hidden && !pausedRef.current;

    const draw = (now: number) => {
      if (now - last < FRAME_MS - 2 && running()) {
        frame = requestAnimationFrame(draw);
        return;
      }
      const seconds = Math.min(0.1, (now - last) / 1000);
      last = now;
      const t = (now - started) / 1000;
      const current = modeRef.current;

      const width = element.clientWidth;
      const height = element.clientHeight;
      const dpr = window.devicePixelRatio || 1;
      if (element.width !== Math.round(width * dpr) || element.height !== Math.round(height * dpr)) {
        element.width = Math.round(width * dpr);
        element.height = Math.round(height * dpr);
      }
      context.setTransform(dpr, 0, 0, dpr, 0, 0);
      context.clearRect(0, 0, width, height);

      envelope.set(current === "idle" ? 0 : (level.current ?? 0));
      const lvl = reduced ? 0 : envelope.step(seconds);
      const ease = 1 - Math.exp(-6 * seconds);
      mix += ((current === "speaking" ? 1 : 0) - mix) * (reduced ? 1 : ease);
      active += ((current === "listening" ? 1 : 0) - active) * (reduced ? 1 : ease);

      const cx = width / 2;
      const cy = height * 0.46;
      const size = Math.min(width, height);

      // A warm wash that grows with the voice.
      const glow = 0.03 + 0.05 * active + 0.07 * lvl;
      const gradient = context.createRadialGradient(cx, cy, 0, cx, cy, size * 0.55);
      gradient.addColorStop(0, `rgba(201,115,77,${glow.toFixed(3)})`);
      gradient.addColorStop(1, "rgba(201,115,77,0)");
      context.fillStyle = gradient;
      context.fillRect(0, 0, width, height);

      if (mix < 0.98) {
        const orbLevel = lvl * (1 - mix);
        const radius = size * 0.3 * (1 + 0.22 * orbLevel);
        const spin = reduced ? 0.6 : t * (0.12 + 0.3 * active);
        const tilt = 0.42;
        const strength =
          (DOT_STRENGTH.idle + (DOT_STRENGTH.active - DOT_STRENGTH.idle) * active) * (1 - mix);
        const [cs, ss, ct, st] = [Math.cos(spin), Math.sin(spin), Math.cos(tilt), Math.sin(tilt)];
        for (const shade of shades) shade.length = 0;
        for (const p of POINTS) {
          const x1 = p.x * cs + p.z * ss;
          const z1 = -p.x * ss + p.z * cs;
          const y1 = p.y * ct - z1 * st;
          const z2 = p.y * st + z1 * ct;
          const wobble = reduced ? 1 : 1 + orbLevel * 0.28 * Math.sin(p.phase + t * 6 + p.y * 4.2);
          const r = radius * wobble;
          const depth = (z2 + 1) / 2;
          shades[Math.min(SHADES - 1, Math.floor(depth * SHADES))].push({
            x: cx + x1 * r,
            y: cy + y1 * r,
            r: 0.8 + 1.8 * depth,
          });
        }
        shades.forEach((dots, i) => {
          if (dots.length === 0) return;
          const depth = (i + 0.5) / SHADES;
          const alpha = (0.15 + 0.85 * depth) * strength;
          const red = Math.round(201 + 31 * depth);
          const green = Math.round(115 + 85 * depth);
          const blue = Math.round(77 + 73 * depth);
          context.fillStyle = `rgba(${red},${green},${blue},${alpha.toFixed(3)})`;
          context.beginPath();
          for (const dot of dots) {
            context.moveTo(dot.x + dot.r, dot.y);
            context.arc(dot.x, dot.y, dot.r, 0, Math.PI * 2);
          }
          context.fill();
        });
      }

      if (mix > 0.02) {
        const w = Math.max(20, size * 0.05);
        const gap = w * 0.75;
        const x0 = cx - (4 * w + 3 * gap) / 2;
        for (let i = 0; i < 4; i += 1) {
          const wiggle = reduced ? 0.5 : 0.5 + 0.5 * Math.sin(t * (7.5 + i * 2.1) + i * 1.3);
          const target = w + (height * 0.5 - w) * Math.min(1, lvl * BAR_WEIGHTS[i] * (0.55 + 0.75 * wiggle));
          bars[i] += (target - bars[i]) * (reduced ? 1 : Math.min(1, ease * 3));
          const h = Math.max(w, bars[i]);
          context.fillStyle = `rgba(241,237,232,${(BAR_ALPHA * mix).toFixed(3)})`;
          context.beginPath();
          context.roundRect(x0 + i * (w + gap), cy - h / 2, w, h, w / 2);
          context.fill();
        }
      }

      if (running()) frame = requestAnimationFrame(draw);
    };

    const resume = () => {
      cancelAnimationFrame(frame);
      if (running()) {
        last = performance.now() - FRAME_MS;
        frame = requestAnimationFrame(draw);
      }
    };
    wake.current = resume;
    redraw.current = () => {
      last = performance.now() - FRAME_MS;
      draw(performance.now());
    };
    document.addEventListener("visibilitychange", resume);
    frame = requestAnimationFrame(draw);
    return () => {
      redraw.current = null;
      wake.current = null;
      cancelAnimationFrame(frame);
      document.removeEventListener("visibilitychange", resume);
    };
  }, [level]);

  useEffect(() => {
    if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) redraw.current?.();
  }, [mode]);

  useEffect(() => {
    if (!paused) wake.current?.();
  }, [paused]);

  return (
    <canvas
      ref={canvas}
      aria-hidden="true"
      className="pointer-events-none absolute inset-0 size-full"
    />
  );
}
