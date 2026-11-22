import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import type { Platform, PostJson } from "@/lib/api";
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

function Summary({ post }: { post: PostJson }) {
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
        <StatusBadge status={post.status} />
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

export function QueueItem({ post, mode, onMode, onSaved, onDeleted, onOpenSlots }: Props) {
  const close = () => onMode("view");
  return (
    <li>
      <Card className="space-y-3 p-4">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <Summary post={post} />
        </div>
        {mode === "view" && <Actions id={post.id} title={post.title} onMode={onMode} />}
        {mode === "edit" && (
          <EditPostForm post={post} onSaved={onSaved} onCancel={close} onOpenSlots={onOpenSlots} />
        )}
        {mode === "delete" && (
          <DeleteConfirm id={post.id} title={post.title} onDeleted={(notice) => onDeleted(post.id, notice)} onCancel={close} />
        )}
      </Card>
    </li>
  );
}
