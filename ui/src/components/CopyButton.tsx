import { useEffect, useState } from "react";
import { CheckIcon, CopyIcon } from "lucide-react";
import { Button } from "@/components/ui/button";

// Copies `text` exactly as given — a fingerprint or an id is compared character by character, so
// nothing here trims, re-cases or re-spaces it.
export function CopyButton({ text, label, withText = false }: { text: string; label: string; withText?: boolean }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const reset = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(reset);
  }, [copied]);

  function copy() {
    void navigator.clipboard?.writeText(text).then(
      () => setCopied(true),
      () => {},
    );
  }

  const icon = copied ? <CheckIcon className="text-success" /> : <CopyIcon />;
  return withText ? (
    <Button variant="outline" size="sm" onClick={copy} aria-label={label}>
      {icon}
      {copied ? "Copied" : "Copy"}
    </Button>
  ) : (
    <Button variant="outline" size="icon-sm" onClick={copy} aria-label={label} title={label}>
      {icon}
    </Button>
  );
}

// A fingerprint as eight tiles. **The text is the fingerprint exactly** — the groups are spans
// with the original single spaces between them, so selecting it or reading its text gives the same
// string the other machine shows, and nothing is ever split inside a group.
export function Fingerprint({ value, size = "lg" }: { value: string; size?: "lg" | "md" }) {
  const groups = value.split(" ");
  return (
    <p className="m-0 grid grid-cols-4 gap-2 font-mono max-[560px]:grid-cols-2">
      {groups.map((group, i) => (
        <span key={i}>
          <span
            className={
              size === "lg"
                ? "block rounded-lg border border-sidebar-border bg-inset py-2.5 text-center text-lg tracking-[0.12em] text-heading"
                : "block rounded-md border border-sidebar-border bg-inset py-2 text-center text-base tracking-[0.12em] text-heading"
            }
          >
            {group}
          </span>
          {i < groups.length - 1 ? " " : null}
        </span>
      ))}
    </p>
  );
}
