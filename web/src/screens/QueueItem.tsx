import { useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import type { Platform, PostJson } from "@/lib/api";
import { formatSlot } from "@/lib/rules";
import { DeleteConfirm } from "@/screens/DeleteConfirm";

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

interface Props {
  post: PostJson;
  onDeleted: (id: string) => void;
}

export function QueueItem({ post, onDeleted }: Props) {
  const [deleting, setDeleting] = useState(false);
  return (
    <li>
      <Card className="space-y-3 p-4">
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <Summary post={post} />
        </div>
        {deleting ? (
          <DeleteConfirm
            id={post.id}
            title={post.title}
            onDeleted={() => onDeleted(post.id)}
            onCancel={() => setDeleting(false)}
          />
        ) : (
          <div className="flex gap-3">
            <Button
              type="button"
              variant="outline"
              className="h-11 min-h-11"
              aria-label={`Delete ${post.title}`}
              onClick={() => setDeleting(true)}
            >
              Delete
            </Button>
          </div>
        )}
      </Card>
    </li>
  );
}
