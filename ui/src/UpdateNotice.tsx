import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button } from "@/components/ui/button";

type Available = { version: string; notes: string | null };

// Asked once when the window opens. A phone build has no such command, and a copy installed from
// the Nix store answers `null`; either way nothing is shown.
export function UpdateNotice() {
  const [available, setAvailable] = useState<Available | null>(null);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    invoke<Available | null>("check_for_update")
      .then(setAvailable)
      .catch(() => setAvailable(null));
  }, []);

  if (!available || dismissed) return null;

  const install = () => {
    setInstalling(true);
    setError(null);
    // Resolves only on failure: success restarts the app into the new version.
    invoke("install_update").catch((reason) => {
      setInstalling(false);
      setError(String(reason));
    });
  };

  return (
    <div
      role="status"
      className="fixed right-4 bottom-4 z-50 flex max-w-sm flex-col gap-3 rounded-xl border bg-card p-4 shadow-lg max-sm:left-4"
    >
      <p className="m-0 text-sm text-heading">Zyris {available.version} is available.</p>
      {error && <p className="m-0 text-xs text-destructive">The update did not install: {error}</p>}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" size="sm" disabled={installing} onClick={() => setDismissed(true)}>
          Later
        </Button>
        <Button size="sm" disabled={installing} onClick={install}>
          {installing ? "Installing…" : "Install and restart"}
        </Button>
      </div>
    </div>
  );
}
