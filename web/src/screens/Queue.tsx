import { useEffect, useState } from "react";
import { getQueue, type PostJson } from "@/lib/api";
import { QueueItem, type Mode } from "@/screens/QueueItem";

type State =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "loaded"; posts: PostJson[] };

export function Queue({ onOpenSlots = () => undefined }: { onOpenSlots?: () => void }) {
  const [state, setState] = useState<State>({ kind: "loading" });
  // One post at a time is open for editing or deleting, so the forms never clash.
  const [active, setActive] = useState<{ id: string; mode: Mode } | null>(null);

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

  const update = (change: (posts: PostJson[]) => PostJson[]) =>
    setState((cur) => (cur.kind === "loaded" ? { kind: "loaded", posts: change(cur.posts) } : cur));
  const remove = (id: string) => {
    update((posts) => posts.filter((p) => p.id !== id));
    setActive(null);
  };
  const replace = (saved: PostJson) => {
    update((posts) => posts.map((p) => (p.id === saved.id ? saved : p)).sort((a, b) => a.slot_at - b.slot_at));
    setActive(null);
  };

  return (
    <ul className="space-y-3">
      {state.posts.map((p) => (
        <QueueItem
          key={p.id}
          post={p}
          mode={active?.id === p.id ? active.mode : "view"}
          onMode={(mode) => setActive(mode === "view" ? null : { id: p.id, mode })}
          onSaved={replace}
          onDeleted={remove}
          onOpenSlots={onOpenSlots}
        />
      ))}
    </ul>
  );
}
