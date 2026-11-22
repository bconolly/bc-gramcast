import { useEffect, useRef, useState } from "react";
import { getQueue, type PostJson } from "@/lib/api";
import { QueueItem, type Mode } from "@/screens/QueueItem";

type State =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "loaded"; posts: PostJson[] };

const EMPTY_ID = "queue-empty";

/** Where focus goes once the next render has put the element on the page. */
function useFocusLater() {
  const target = useRef<string | null>(null);
  useEffect(() => {
    const id = target.current;
    target.current = null;
    if (id) document.getElementById(id)?.focus();
  });
  return (id: string) => {
    target.current = id;
  };
}

/** The row that takes focus when `id` is deleted: the next one, else the previous one. */
function neighbourEditId(posts: PostJson[], id: string): string {
  const i = posts.findIndex((p) => p.id === id);
  const rest = posts.filter((p) => p.id !== id);
  const next = rest[i] ?? rest[i - 1];
  return next ? `edit-${next.id}` : EMPTY_ID;
}

export function Queue({ onOpenSlots = () => undefined }: { onOpenSlots?: () => void }) {
  const [state, setState] = useState<State>({ kind: "loading" });
  // One post at a time is open for editing or deleting, so the forms never clash.
  const [active, setActive] = useState<{ id: string; mode: Mode } | null>(null);
  const [notice, setNotice] = useState("");
  const focusLater = useFocusLater();

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

  const posts = state.kind === "loaded" ? state.posts : [];
  const update = (change: (posts: PostJson[]) => PostJson[]) =>
    setState((cur) => (cur.kind === "loaded" ? { kind: "loaded", posts: change(cur.posts) } : cur));

  function setMode(id: string, mode: Mode) {
    setNotice("");
    // Closing a form puts focus back on the button that opened it.
    if (mode === "view" && active) focusLater(`${active.mode}-${id}`);
    setActive(mode === "view" ? null : { id, mode });
  }
  function remove(id: string, message = "Post deleted") {
    focusLater(neighbourEditId(posts, id));
    update((cur) => cur.filter((p) => p.id !== id));
    setActive(null);
    setNotice(message);
  }
  function replace(saved: PostJson) {
    focusLater(`edit-${saved.id}`);
    update((cur) => cur.map((p) => (p.id === saved.id ? saved : p)).sort((a, b) => a.slot_at - b.slot_at));
    setActive(null);
    setNotice("Post saved");
  }

  let body;
  if (state.kind === "loading") body = <p className="text-muted-foreground">Loading the queue…</p>;
  else if (state.kind === "error") {
    body = (
      <p role="alert" className="text-destructive">
        {state.message}
      </p>
    );
  } else if (posts.length === 0) {
    body = (
      <p id={EMPTY_ID} tabIndex={-1} className="text-muted-foreground">
        Nothing queued yet.
      </p>
    );
  } else {
    body = (
      <ul className="space-y-3">
        {posts.map((p) => (
          <QueueItem
            key={p.id}
            post={p}
            mode={active?.id === p.id ? active.mode : "view"}
            onMode={(mode) => setMode(p.id, mode)}
            onSaved={replace}
            onDeleted={remove}
            onOpenSlots={onOpenSlots}
          />
        ))}
      </ul>
    );
  }

  return (
    <>
      {body}
      <p role="status" aria-live="polite" className="sr-only">
        {notice}
      </p>
    </>
  );
}
