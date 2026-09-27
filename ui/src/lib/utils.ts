import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

// Class names joined, with later Tailwind utilities winning over earlier ones that set the same
// property — so a component's `className` prop can override its defaults. The shadcn convention.
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
