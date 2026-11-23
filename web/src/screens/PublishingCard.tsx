import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import {
  PUBLISH_PRIVACIES,
  getPublishSettings,
  isSettingsConflict,
  savePublishSettings,
  type PublishPrivacy,
  type PublishSettings,
} from "@/lib/api";

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready"; stored: PublishSettings };

const messageOf = (e: unknown) => (e instanceof Error ? e.message : "Something went wrong.");

const CONFLICT_NOTICE =
  "These settings were changed since you opened them. They have been reloaded, so check them and save again.";

const PRIVACY_LABELS: Record<PublishPrivacy, string> = {
  private: "Private (only you)",
  unlisted: "Unlisted (anyone with the link)",
  public: "Public (anyone can find it)",
};

/** In-page question before a public save, like the delete question: Escape or Cancel closes it. */
function PublicConfirm({ busy, onConfirm, onCancel }: { busy: boolean; onConfirm: () => void; onCancel: () => void }) {
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") onCancel();
  };
  return (
    <div
      role="alertdialog"
      aria-labelledby="public-confirm-title"
      aria-describedby="public-confirm-body"
      onKeyDown={onKeyDown}
      className="space-y-3 border-t pt-3"
    >
      <p id="public-confirm-title" className="text-sm font-medium">
        Post videos as public?
      </p>
      <p id="public-confirm-body" className="text-sm">
        Anyone on YouTube will be able to find and watch the videos Gramcast posts while this is on.
      </p>
      <div className="flex flex-wrap gap-3">
        <Button type="button" className="h-11 min-h-11" disabled={busy} onClick={onConfirm}>
          Save as public
        </Button>
        <Button type="button" variant="outline" className="h-11 min-h-11" onClick={onCancel} autoFocus>
          Cancel
        </Button>
      </div>
    </div>
  );
}

export function PublishingCard() {
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  const [enabled, setEnabled] = useState(false);
  const [privacy, setPrivacy] = useState<PublishPrivacy>("private");
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const returnFocus = useRef(false);
  const saveRef = useRef<HTMLButtonElement>(null);

  const show = useCallback((stored: PublishSettings) => {
    setLoad({ kind: "ready", stored });
    setEnabled(stored.enabled);
    setPrivacy(stored.privacy);
  }, []);

  const reload = useCallback(async () => {
    setLoad({ kind: "loading" });
    try {
      show(await getPublishSettings());
    } catch (e) {
      setLoad({ kind: "error", message: messageOf(e) });
    }
  }, [show]);

  // Initial load: state is set only when the request settles, as on the Connections screen.
  useEffect(() => {
    let live = true;
    getPublishSettings().then(
      (stored) => live && show(stored),
      (e: unknown) => live && setLoad({ kind: "error", message: messageOf(e) }),
    );
    return () => {
      live = false;
    };
  }, [show]);

  // After the question closes, focus goes back to the Save button once it can take it.
  useEffect(() => {
    if (returnFocus.current && !busy && !confirming) {
      returnFocus.current = false;
      saveRef.current?.focus();
    }
  }, [busy, confirming]);

  async function save() {
    if (load.kind !== "ready") return;
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const stored = await savePublishSettings({ enabled, privacy, expected_updated_at: load.stored.updated_at });
      show(stored);
      setNotice("Publishing settings saved.");
    } catch (e) {
      if (isSettingsConflict(e)) {
        setError(CONFLICT_NOTICE);
        await reload();
      } else {
        setError(messageOf(e));
      }
    } finally {
      setBusy(false);
    }
  }

  function onSaveClick() {
    if (privacy === "public") setConfirming(true);
    else void save();
  }

  function closeConfirm() {
    setConfirming(false);
    returnFocus.current = true;
  }

  function confirmPublic() {
    closeConfirm();
    void save();
  }

  return (
    <Card className="space-y-3 p-4">
      <h3 className="font-medium">Publishing</h3>
      {load.kind === "loading" && <p className="text-muted-foreground text-sm">Loading…</p>}
      {load.kind === "error" && (
        <div className="space-y-2">
          <p role="alert" className="text-destructive text-sm">
            {load.message}
          </p>
          <Button type="button" variant="outline" className="min-h-11" onClick={() => void reload()}>
            Retry
          </Button>
        </div>
      )}
      {load.kind === "ready" && (
        <div className="space-y-3">
          <div className="flex min-h-11 items-center justify-between gap-3">
            <span id="publishing-switch-label" className="text-sm font-medium">
              Publishing
            </span>
            <div className="flex items-center gap-2">
              <span aria-hidden="true" className="text-muted-foreground text-sm">
                {enabled ? "On" : "Off"}
              </span>
              <Switch aria-labelledby="publishing-switch-label" checked={enabled} onCheckedChange={setEnabled} disabled={busy} />
            </div>
          </div>
          <div className="space-y-1">
            <Label htmlFor="publish-privacy">Privacy</Label>
            <Select id="publish-privacy" value={privacy} disabled={busy} onChange={(e) => setPrivacy(e.target.value as PublishPrivacy)}>
              {PUBLISH_PRIVACIES.map((p) => (
                <option key={p} value={p}>
                  {PRIVACY_LABELS[p]}
                </option>
              ))}
            </Select>
          </div>
          {load.stored.source === "defaults" && (
            <p className="text-muted-foreground text-xs">
              Nothing has been saved yet, so the settings made when Gramcast was deployed are in use.
            </p>
          )}
          {error && (
            <p role="alert" className="text-destructive text-sm">
              {error}
            </p>
          )}
          {notice && (
            <p role="status" className="text-sm">
              {notice}
            </p>
          )}
          <Button ref={saveRef} type="button" className="min-h-11" disabled={busy} onClick={onSaveClick}>
            {busy ? "Saving…" : "Save"}
          </Button>
          {confirming && <PublicConfirm busy={busy} onConfirm={confirmPublic} onCancel={closeConfirm} />}
        </div>
      )}
    </Card>
  );
}
