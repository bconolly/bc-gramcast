import { useEffect, useState } from "react";
import { getQueue, type PostJson } from "@/lib/api";
import { QueueItem } from "@/screens/QueueItem";

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

  const remove = (id: string) =>
    setState((cur) => (cur.kind === "loaded" ? { kind: "loaded", posts: cur.posts.filter((p) => p.id !== id) } : cur));

  return (
    <ul className="space-y-3">
      {state.posts.map((p) => (
        <QueueItem key={p.id} post={p} onDeleted={remove} />
      ))}
    </ul>
  );
}
