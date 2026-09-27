import type * as React from "react";
import { cn } from "@/lib/utils";

// The small square an icon sits in at the head of a card.
export function IconTile({
  children,
  tone = "accent",
  className,
}: {
  children: React.ReactNode;
  tone?: "accent" | "success" | "destructive" | "muted";
  className?: string;
}) {
  return (
    <span
      aria-hidden="true"
      className={cn(
        "inline-flex size-9 shrink-0 items-center justify-center rounded-[0.5625rem] border [&_svg]:size-[1.125rem]",
        tone === "accent" && "border-border bg-muted text-primary",
        tone === "success" && "border-success/30 bg-success/10 text-[#a9c99a]",
        tone === "destructive" && "border-border bg-muted text-[#ef7d75]",
        tone === "muted" && "border-border bg-muted text-subtle",
        className,
      )}
    >
      {children}
    </span>
  );
}

// A line saying something went wrong, in the card it went wrong in.
export function Problem({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <p role="alert" className={cn("m-0 text-[0.8125rem] text-[#ef7d75]", className)}>
      {children}
    </p>
  );
}

// A line of secondary information.
export function Note({ children, className }: { children: React.ReactNode; className?: string }) {
  return <p className={cn("m-0 text-[0.8125rem] text-muted-foreground", className)}>{children}</p>;
}

// Something a person may want to copy, in the monospace face, never re-cased or re-spaced.
export function Mono({ children, className }: { children: React.ReactNode; className?: string }) {
  return <span className={cn("font-mono text-[0.8125rem] [overflow-wrap:anywhere]", className)}>{children}</span>;
}
