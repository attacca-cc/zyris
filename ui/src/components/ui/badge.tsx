import type * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "@/lib/utils";

const badgeVariants = cva(
  "inline-flex w-fit shrink-0 items-center gap-1 whitespace-nowrap rounded-full border px-2 py-0.5 text-[0.71875rem] font-medium [&>svg]:size-3",
  {
    variants: {
      variant: {
        default: "border-primary/35 bg-primary/10 text-[#e3a283]",
        secondary: "border-border bg-muted text-muted-foreground",
        success: "border-success/30 bg-success/10 text-[#a9c99a]",
        warning: "border-warning/30 bg-warning/10 text-[#e0b25c]",
        destructive: "border-destructive/30 bg-destructive/10 text-[#ef7d75]",
      },
    },
    defaultVariants: { variant: "secondary" },
  },
);

function Badge({
  className,
  variant,
  ...props
}: React.ComponentProps<"span"> & VariantProps<typeof badgeVariants>) {
  return <span data-slot="badge" className={cn(badgeVariants({ variant }), className)} {...props} />;
}

export { Badge, badgeVariants };
