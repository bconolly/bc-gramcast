import { useRef, useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { isSlotTaken, submitPost, type Platform } from "@/lib/api";
import {
  MAX_CAPTION_CHARS,
  MAX_TITLE_CHARS,
  charCount,
  localInputToUnix,
  validateForm,
} from "@/lib/rules";
import { CountedField, PlatformPicker, Problems } from "@/screens/PostFields";
import { WhenField } from "@/screens/WhenField";
import { useSlotOptions } from "@/screens/useSlotOptions";

export function Upload({ onQueued, onOpenSlots }: { onQueued: () => void; onOpenSlots: () => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [title, setTitle] = useState("");
  const [caption, setCaption] = useState("");
  const [platforms, setPlatforms] = useState<Platform[]>([]);
  const [when, setWhen] = useState("");
  const [problems, setProblems] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const slotOptions = useSlotOptions(when !== "" || editing);
  // Key of the video already in storage for the chosen file, so a failed queue
  // call retries without uploading again. A new file clears it. The upload link
  // itself is never kept.
  const uploadedKey = useRef<string | undefined>(undefined);

  function toggle(id: Platform, on: boolean) {
    setPlatforms((cur) => (on ? [...cur, id] : cur.filter((p) => p !== id)));
  }

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const slotAt =
      slotOptions.options.status === "ready" && slotOptions.choice !== null
        ? slotOptions.choice
        : localInputToUnix(when);
    const found = validateForm({
      file,
      title,
      caption,
      platforms,
      slotAt,
      now: Math.floor(Date.now() / 1000),
    });
    setProblems(found);
    if (found.length > 0 || !file || slotAt === null) return;
    setBusy(true);
    try {
      await submitPost(
        { file, title, caption, platforms, slot_at: slotAt },
        { videoKey: uploadedKey.current, onUploaded: (k) => (uploadedKey.current = k) },
      );
      onQueued();
    } catch (err) {
      const message = err instanceof Error ? err.message : "Something went wrong.";
      setProblems([message]);
      if (isSlotTaken(err) && (await slotOptions.refreshAfterTaken())) {
        setProblems([message, "We have picked the next free slot."]);
      }
      setBusy(false);
    }
  }

  return (
    <form onSubmit={onSubmit} noValidate className="space-y-5">
      <div className="space-y-2">
        <Label htmlFor="video">Video (MP4)</Label>
        <Input
          id="video"
          type="file"
          accept=".mp4,video/mp4"
          onChange={(e) => {
            uploadedKey.current = undefined;
            setFile(e.target.files?.[0] ?? null);
          }}
        />
      </div>
      <CountedField id="title" label="Title" n={charCount(title)} max={MAX_TITLE_CHARS}>
        <Input id="title" value={title} onChange={(e) => setTitle(e.target.value)} />
      </CountedField>
      <CountedField id="caption" label="Caption" n={charCount(caption)} max={MAX_CAPTION_CHARS}>
        <Textarea id="caption" value={caption} onChange={(e) => setCaption(e.target.value)} />
      </CountedField>
      <PlatformPicker selected={platforms} onToggle={toggle} />
      <WhenField
        options={slotOptions.options}
        choice={slotOptions.choice}
        onChoice={slotOptions.setChoice}
        when={when}
        onWhen={setWhen}
        onEditing={setEditing}
        disabled={busy}
        onOpenSlots={onOpenSlots}
      />
      <Problems problems={problems} />
      <Button type="submit" disabled={busy} className="w-full sm:w-auto">
        {busy ? "Queueing…" : "Queue post"}
      </Button>
    </form>
  );
}
