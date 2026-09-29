import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SmartphoneIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { IconTile, Note, Problem } from "@/components/IconTile";

type Access = { touch: boolean; screen: boolean; allFiles: boolean; installs: boolean };

// What this phone lends its agent, each behind a switch only Android can flip: the screen (asked
// once per session), touch (an accessibility service), and files beyond the app's own. Read again
// whenever the window comes back, since each is changed in the system settings.
export function PhoneAccess() {
  const [access, setAccess] = useState<Access | null>(null);
  const [problem, setProblem] = useState<string | null>(null);

  const read = useCallback(() => {
    invoke<Access>("phone_status")
      .then((next) => {
        setAccess(next);
        setProblem(null);
      })
      .catch((reason) => setProblem(String(reason)));
  }, []);

  useEffect(() => {
    read();
    const onFocus = () => document.visibilityState === "visible" && read();
    document.addEventListener("visibilitychange", onFocus);
    return () => document.removeEventListener("visibilitychange", onFocus);
  }, [read]);

  const run = (command: string) => () => {
    invoke(command)
      .then(read)
      .catch((reason) => setProblem(String(reason)));
  };

  const rows: { label: string; on: boolean | undefined; action: string; command: string }[] = [
    { label: "See the screen", on: access?.screen, action: "Allow", command: "phone_allow_screen" },
    { label: "Tap, swipe and type", on: access?.touch, action: "Open settings", command: "phone_open_touch_settings" },
    { label: "Files outside the app", on: access?.allFiles, action: "Open settings", command: "phone_open_files_settings" },
  ];

  return (
    <Card>
      <CardHeader className="items-center">
        <IconTile>
          <SmartphoneIcon />
        </IconTile>
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <CardTitle>What your agent can use</CardTitle>
          <CardDescription>Android asks you before each of these. Turn on only what you want your agent to have.</CardDescription>
        </div>
      </CardHeader>
      {problem ? (
        <Problem>{problem}</Problem>
      ) : (
        <div className="flex flex-col gap-2">
          {rows.map((row) => (
            <div key={row.command} className="flex items-center justify-between gap-3 text-sm">
              <span className="text-heading">{row.label}</span>
              {row.on ? (
                <span className="text-muted-foreground">On</span>
              ) : (
                <Button size="sm" variant="outline" disabled={access === null} onClick={run(row.command)}>
                  {row.action}
                </Button>
              )}
            </div>
          ))}
          <Note className="text-xs text-subtle">
            Touch is an accessibility service: find Zyris under Installed apps in Accessibility and turn it on.
          </Note>
        </div>
      )}
    </Card>
  );
}
