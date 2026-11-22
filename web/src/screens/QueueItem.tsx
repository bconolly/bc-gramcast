import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import type { Platform, PostJson } from "@/lib/api";
import { failureMessage, youtubeShortsUrl, type PublishInfo } from "@/lib/publish";
import { formatSlot } from "@/lib/rules";
import { DeleteConfirm } from "@/screens/DeleteConfirm";
import { EditPostForm } from "@/screens/EditPostForm";

const PLATFORM_NAMES: Record<Platform, string> = { instagram: "Instagram", youtube: "YouTube" };

function StatusBadge({ status }: { status: PostJson["status"] }) {
  return status === "ready" ? (
    <Badge variant="success">Ready</Badge>
  ) : (
    <Badge variant="warning">Needs caption</Badge>
  );
}

const QUEUED: PublishInfo = { state: "queued" };

/** The publish state as a badge, plus a link once published. Nothing here is built from server text but the id. */
function PublishBadge({ publish }: { publish: PublishInfo }) {
  switch (publish.state) {
    case "queued":
      return <Badge variant="secondary">Queued</Badge>;
    case "publishing":
      return <Badge variant="warning">Publishing</Badge>;
    case "published":
      return <Badge variant="success">Published</Badge>;
    default:
      return <Badge variant="warning">Failed</Badge>;
  }
}

function PublishDetail({ publish }: { publish: PublishInfo }) {
  if (publish.state === "failed") {
    return <p className="text-sm">{failureMessage(publish.reason)}</p>;
  }
  const url = publish.state === "published" ? youtubeShortsUrl(publish.external_id) : null;
  if (!url) return null;
  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      className="inline-flex min-h-11 items-center text-sm underline"
    >
      Watch on YouTube <span className="sr-only">(opens in new tab)</span>
    </a>
  );
}

function Summary({ post }: { post: PostJson }) {
  const publish = post.publish ?? QUEUED;
  return (
    <>
      <div className="min-w-0">
        <p className="truncate font-medium">{post.title}</p>
        <p className="text-sm text-muted-foreground">
          {post.platforms.map((x) => (
            <span key={x} className="mr-3">
              {PLATFORM_NAMES[x]}
            </span>
          ))}
        </p>
      </div>
      <div className="flex items-center gap-3">
        <time className="text-sm tabular-nums">{formatSlot(post.slot_at)}</time>
        <PublishBadge publish={publish} />
        {publish.state === "queued" && <StatusBadge status={post.status} />}
      </div>
    </>
  );
}

export type Mode = "view" | "edit" | "delete";

interface Props {
  post: PostJson;
  mode: Mode;
  onMode: (mode: Mode) => void;
  onSaved: (post: PostJson) => void;
  onDeleted: (id: string, notice?: string) => void;
  onOpenSlots: () => void;
}

function Actions({ id, title, onMode }: { id: string; title: string; onMode: (mode: Mode) => void }) {
  return (
    <div className="flex gap-3">
      <Button type="button" variant="outline" className="h-11 min-h-11" id={`edit-${id}`}
        aria-label={`Edit ${title}`} onClick={() => onMode("edit")}>
        Edit
      </Button>
      <Button type="button" variant="outline" className="h-11 min-h-11" id={`delete-${id}`}
        aria-label={`Delete ${title}`} onClick={() => onMode("delete")}>
        Delete
      </Button>
    </div>
  );
}

/** Only a post that is still queued, with its slot ahead, can be changed (the Worker enforces it too). */
export const canChange = (post: PostJson) =>
  (post.publish ?? QUEUED).state === "queued" && post.slot_at > Date.now() / 1000;

export function QueueItem({ post, mode, onMode, onSaved, onDeleted, onOpenSlots }: Props) {
  const close = () => onMode("view");
  const publish = post.publish ?? QUEUED;
  return (
    <li>
      <Card className="space-y-3 p-4">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <Summary post={post} />
        </div>
        <PublishDetail publish={publish} />
        {mode === "view" && canChange(post) && <Actions id={post.id} title={post.title} onMode={onMode} />}
        {mode === "edit" && (
          <EditPostForm post={post} onSaved={onSaved}
            onCancel={close}
            onGone={(notice) => onDeleted(post.id, notice)}
            onOpenSlots={onOpenSlots} />
        )}
        {mode === "delete" && (
          <DeleteConfirm id={post.id} title={post.title} onDeleted={(notice) => onDeleted(post.id, notice)} onCancel={close} />
        )}
      </Card>
    </li>
  );
}
