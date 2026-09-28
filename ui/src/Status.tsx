import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { KeyRoundIcon, WifiIcon, WifiOffIcon } from "lucide-react";
import { PHONE, type State } from "./state";
import { PhoneAccess } from "./PhoneAccess";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { CopyButton, Fingerprint } from "@/components/CopyButton";
import { IconTile, Mono, Note, Problem } from "@/components/IconTile";
import { Page, PageHeader } from "@/components/PageHeader";

// This computer's own peer fingerprint, as the `peer_fingerprint` command answers with it — the
// same string `Peering::fingerprint` hands the approval screen about somebody else's machine, and
// the same one that machine's own Status screen shows about this one.
//
// Three answers, like the inbox read on the Tools screen and for the same reason. `undefined` is a
// read still in flight, `null` is a computer with no peer identity — no key, no `file_transfer`,
// nothing to compare — and a string is the value. An empty string is none of those and must never
// be shown as a fingerprint made of nothing.
type Fingerprint = string | null | undefined;

export function Status({ state }: { state: State }) {
  // Which Zyris this is, for a bug report and for knowing whether an update has landed.
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(null));
  }, []);
  const [fingerprint, setFingerprint] = useState<Fingerprint>(undefined);
  const [fingerprintProblem, setFingerprintProblem] = useState<string | null>(null);

  // Mount only, and that is the whole of it: this is computed once when the endpoint binds and
  // cannot change while the process runs — the same key is the same fingerprint, which is the
  // property the key is persisted for. Nothing to re-read on focus the way Settings has to.
  useEffect(() => {
    // The same guard the other screens use: StrictMode runs this effect twice and the answer can
    // land after the cleanup.
    let cancelled = false;

    void invoke<string | null>("peer_fingerprint")
      .then((answer) => {
        if (!cancelled) setFingerprint(answer);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        // Never `setFingerprint(null)` on a failed read. "This computer has no peer identity" is a
        // sentence about the machine; a read that did not come back says nothing at all, and the
        // one thing this screen must not do is answer a question it could not read.
        setFingerprintProblem(
          typeof error === "string" ? error : "Could not read this computer's fingerprint.",
        );
      });

    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <Page>
      <PageHeader
        title="Status"
        description={`${PHONE ? "This phone's" : "This computer's"} link to Attacca.${version ? ` Zyris ${version}.` : ""}`}
      />

      <Card
        className={
          state.connected
            ? "bg-[radial-gradient(500px_200px_at_0%_0%,rgba(127,176,105,0.08),transparent_70%),var(--card)]"
            : undefined
        }
      >
        <CardHeader className="items-center">
          <IconTile tone={state.connected ? "success" : "muted"} className="size-11 rounded-xl [&_svg]:size-5">
            {state.connected ? <WifiIcon /> : <WifiOffIcon />}
          </IconTile>
          <div className="flex flex-col gap-0.5">
            <h2 className="m-0 text-lg font-semibold text-heading">{state.connected ? "Connected" : "Not connected"}</h2>
            <CardDescription>
              {state.connected ? "Your agents can reach this computer." : "Your agents cannot reach this computer right now."}
            </CardDescription>
          </div>
        </CardHeader>

        {!state.connected && state.problem && (
          <div className="flex flex-col gap-1">
            <Problem>{state.problem}</Problem>
            <Note>
              {state.retrying ? "Zyris keeps trying on its own." : "Zyris has stopped trying. Restart it to reconnect."}
            </Note>
          </div>
        )}

        {state.node ? (
          <dl className="m-0 grid grid-cols-[8rem_minmax(0,1fr)] items-center gap-x-4 gap-y-3 border-t border-sidebar-border pt-4 text-[0.84375rem]">
            <dt className="text-muted-foreground">Computer name</dt>
            <dd className="m-0 text-heading">{state.node.nodeName}</dd>
            <dt className="text-muted-foreground">Node ID</dt>
            <dd className="m-0 flex min-w-0 items-center gap-2">
              <Mono className="truncate text-foreground">{state.node.nodeId}</Mono>
              <CopyButton text={state.node.nodeId} label="Copy the node ID" />
            </dd>
          </dl>
        ) : (
          <Note>This computer has not connected yet.</Note>
        )}
      </Card>

      {/* **Apart from the node id above, on purpose.** Those come from Attacca and name this node
          on the account; this comes from a key file on this disk and names this machine to its
          peers. It works before this machine has ever connected, so it is not inside the node
          branch — and keeping it separate stops anybody reading the node id aloud against a
          fingerprint. */}
      {PHONE ? (
        <PhoneAccess />
      ) : (
        <Card>
          <CardHeader className="items-center">
            <IconTile>
              <KeyRoundIcon />
            </IconTile>
            <div className="flex min-w-0 flex-1 flex-col gap-0.5">
              <CardTitle>This computer's fingerprint</CardTitle>
              <CardDescription>Read this out when another of your machines asks you to approve this one.</CardDescription>
            </div>
            {typeof fingerprint === "string" && !fingerprintProblem && (
              <CopyButton text={fingerprint} label="Copy the fingerprint" withText />
            )}
          </CardHeader>
          {fingerprintProblem ? (
            <Problem>{fingerprintProblem}</Problem>
          ) : fingerprint === undefined ? (
            <Note>Reading this computer's fingerprint.</Note>
          ) : fingerprint === null ? (
            <Note>
              File transfer is not running on this computer, so it has no fingerprint and no other machine can
              send a file here. Zyris says why in its log when it starts.
            </Note>
          ) : (
            <>
              <Fingerprint value={fingerprint} />
              <Note className="text-xs text-subtle">
                Not a secret — it is the short form of this computer's public key, and it stays the same after
                every restart.
              </Note>
            </>
          )}
        </Card>
      )}
    </Page>
  );
}
