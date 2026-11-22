import { useState } from "react";
import { Button } from "@/components/ui/button";
import { deletePost } from "@/lib/api";

interface Props {
  id: string;
  title: string;
  onDeleted: () => void;
  onCancel: () => void;
}

/** In-page confirmation: deleting removes the post and its video for good. */
export function DeleteConfirm({ id, title, onDeleted, onCancel }: Props) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      await deletePost(id);
      onDeleted();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Something went wrong.");
      setBusy(false);
    }
  }

  return (
    <div role="group" aria-label={`Confirm delete of ${title}`} className="space-y-3 border-t pt-3">
      <p className="text-sm">
        Delete this post and its video? This cannot be undone.
      </p>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <div className="flex flex-wrap gap-3">
        <Button type="button" variant="destructive" className="h-11 min-h-11" disabled={busy} onClick={confirm}>
          {busy ? "Deleting…" : "Delete post"}
        </Button>
        <Button type="button" variant="outline" className="h-11 min-h-11" disabled={busy} onClick={onCancel} autoFocus>
          Cancel
        </Button>
      </div>
    </div>
  );
}
