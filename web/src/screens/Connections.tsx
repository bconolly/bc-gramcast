import { useEffect, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { buttonVariants } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { getConnectStatus, type ConnectStatus } from "@/lib/api";
import { YOUTUBE_CONNECT_PATH, type ConnectResult } from "@/lib/connect";
import { cn } from "@/lib/utils";

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; status: ConnectStatus };

const messageOf = (e: unknown) => (e instanceof Error ? e.message : "Something went wrong.");

function YoutubeState({ load }: { load: Load }) {
  if (load.kind === "loading") return <p className="text-muted-foreground text-sm">Checking…</p>;
  if (load.kind === "error") {
    return (
      <p role="alert" className="text-destructive text-sm">
        {load.message}
      </p>
    );
  }
  const { connected, needs_reconnect } = load.status.youtube;
  if (!connected) return <Badge variant="secondary">Not connected</Badge>;
  if (needs_reconnect) {
    return (
      <div className="space-y-2">
        <Badge variant="warning">Needs reconnecting</Badge>
        <p className="text-sm">
          Google stopped accepting the saved sign-in, so videos will not be posted to YouTube until you reconnect.
        </p>
      </div>
    );
  }
  return <Badge variant="success">Connected</Badge>;
}

export function Connections({ result }: { result: ConnectResult }) {
  const [load, setLoad] = useState<Load>({ kind: "loading" });

  useEffect(() => {
    let live = true;
    getConnectStatus().then(
      (status) => live && setLoad({ kind: "ready", status }),
      (e: unknown) => live && setLoad({ kind: "error", message: messageOf(e) }),
    );
    return () => {
      live = false;
    };
  }, []);

  const connected = load.kind === "ready" && load.status.youtube.connected;
  return (
    <div className="space-y-4">
      <h2 className="text-base font-semibold">Connections</h2>
      {result.errorMessage && (
        <p role="alert" className="border-destructive text-destructive rounded-md border p-3 text-sm">
          {result.errorMessage}
        </p>
      )}
      {result.connected && (
        <p role="status" className="rounded-md border border-emerald-300 bg-emerald-50 p-3 text-sm text-emerald-900">
          YouTube is connected.
        </p>
      )}
      <Card className="space-y-3 p-4">
        <h3 className="font-medium">YouTube</h3>
        <YoutubeState load={load} />
        <p className="text-muted-foreground text-xs">
          Gramcast asks Google only for permission to upload videos. Nothing else is requested.
        </p>
        {/* A plain link: connecting is a full-page trip to Google and back, not an API call. */}
        <a href={YOUTUBE_CONNECT_PATH} className={cn(buttonVariants(), "min-h-11")}>
          {connected ? "Reconnect YouTube" : "Connect YouTube"}
        </a>
      </Card>
    </div>
  );
}
