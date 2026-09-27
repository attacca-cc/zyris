import type * as React from "react";
import { cn } from "@/lib/utils";

// The title and one line under it that every screen but Conversation opens with.
export function PageHeader({
  title,
  description,
  action,
  className,
}: {
  title: React.ReactNode;
  description?: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <header className={cn("mb-2 flex items-end gap-4", className)}>
      <div className="flex min-w-0 flex-1 flex-col gap-1.5">
        <h1 className="m-0 font-display text-[1.625rem] font-semibold tracking-tight text-heading">
          {title}
        </h1>
        {description && <p className="m-0 text-sm text-muted-foreground">{description}</p>}
      </div>
      {action}
    </header>
  );
}

// The scrolling column a settings-like screen is laid out in.
export function Page({ children, wide = false }: { children: React.ReactNode; wide?: boolean }) {
  return (
    <main className="min-w-0 flex-1 overflow-y-auto">
      <div
        className={cn(
          "mx-auto flex flex-col gap-4 px-8 py-10 max-[820px]:px-5",
          wide ? "max-w-[54rem]" : "max-w-[47.5rem]",
        )}
      >
        {children}
      </div>
    </main>
  );
}
