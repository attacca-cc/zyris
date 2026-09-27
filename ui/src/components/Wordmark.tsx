import { useId } from "react";
import { cn } from "@/lib/utils";

// The mark from zyris.attacca.cc (`public/favicon.svg`): two rails and a link between them,
// cropped to the drawing. Gradient ids are per instance, since two marks on one page would
// otherwise share — and fight over — one definition.
export function Mark({ className }: { className?: string }) {
  const id = useId();
  return (
    <svg viewBox="-16 45 254 132" aria-hidden="true" className={cn("h-[0.9375rem] w-auto", className)}>
      <defs>
        <linearGradient id={`${id}l`} gradientUnits="userSpaceOnUse" x1="-13" y1="0" x2="129.5" y2="0">
          <stop offset="0" stopColor="#c9734d" stopOpacity="0" />
          <stop offset="0.4" stopColor="#c9734d" />
        </linearGradient>
        <linearGradient id={`${id}r`} gradientUnits="userSpaceOnUse" x1="92.5" y1="0" x2="235" y2="0">
          <stop offset="0.6" stopColor="#c9734d" />
          <stop offset="1" stopColor="#c9734d" stopOpacity="0" />
        </linearGradient>
      </defs>
      <g
        transform="translate(0,51)"
        fill="none"
        stroke="#c9734d"
        strokeWidth="13"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M-13 21.5 H129.5" stroke={`url(#${id}l)`} />
        <path d="M92.5 98.5 H235" stroke={`url(#${id}r)`} />
        <path d="M135.51 36.41 L86.49 83.59" />
        <circle cx="151" cy="21.5" r="15" />
        <circle cx="71" cy="98.5" r="15" />
      </g>
    </svg>
  );
}

export function Wordmark({ className }: { className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-2.5", className)}>
      <Mark className="h-[0.9375rem]" />
      <span className="font-display text-[1.0625rem] font-semibold tracking-tight text-heading">
        Zyris
      </span>
    </span>
  );
}
