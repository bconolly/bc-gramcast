import { useEffect, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import { getQueue, type Platform, type PostJson } from "@/lib/api";
import { formatSlot } from "@/lib/rules";

const PLATFORM_NAMES: Record<Platform, string> = { instagram: "Instagram", youtube: "YouTube" };

function StatusBadge({ status }: { status: PostJson["status"] }) {
  return status === "ready" ? (
    <Badge variant="success">Ready</Badge>
  ) : (
    <Badge variant="warning">Needs caption</Badge>
  );
}

type State =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "loaded"; posts: PostJson[] };

export function Queue() {
  const [state, setState] = useState<State>({ kind: "loading" });

  useEffect(() => {
    let live = true;
    getQueue().then(
      (posts) => live && setState({ kind: "loaded", posts }),
      (e: unknown) =>
        live &&
        setState({ kind: "error", message: e instanceof Error ? e.message : "Something went wrong." }),
    );
    return () => {
      live = false;
    };
  }, []);

  if (state.kind === "loading") return <p className="text-muted-foreground">Loading the queue…</p>;
  if (state.kind === "error") {
    return (
      <p role="alert" className="text-destructive">
        {state.message}
      </p>
    );
  }
  if (state.posts.length === 0) return <p className="text-muted-foreground">Nothing queued yet.</p>;

  return (
    <ul className="space-y-3">
      {state.posts.map((p) => (
        <li key={p.id}>
          <Card className="flex flex-col gap-2 p-4 sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0">
              <p className="truncate font-medium">{p.title}</p>
              <p className="text-sm text-muted-foreground">
                {p.platforms.map((x) => (
                  <span key={x} className="mr-3">
                    {PLATFORM_NAMES[x]}
                  </span>
                ))}
              </p>
            </div>
            <div className="flex items-center gap-3">
              <time className="text-sm tabular-nums">{formatSlot(p.slot_at)}</time>
              <StatusBadge status={p.status} />
            </div>
          </Card>
        </li>
      ))}
    </ul>
  );
}
