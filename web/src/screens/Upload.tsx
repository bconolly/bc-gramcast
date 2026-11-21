import { useRef, useState, type FormEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { ApiError, submitPost, type Platform } from "@/lib/api";
import {
  MAX_CAPTION_CHARS,
  MAX_TITLE_CHARS,
  charCount,
  localInputToUnix,
  validateForm,
} from "@/lib/rules";
import { WhenField } from "@/screens/WhenField";
import { useSlotOptions } from "@/screens/useSlotOptions";

const PLATFORMS: { id: Platform; name: string }[] = [
  { id: "instagram", name: "Instagram" },
  { id: "youtube", name: "YouTube" },
];

function Counter({ n, max }: { n: number; max: number }) {
  const over = n > max;
  return (
    <span className={over ? "text-destructive" : "text-muted-foreground"}>
      {n.toLocaleString("en-AU")} / {max.toLocaleString("en-AU")}
    </span>
  );
}

function CountedField({
  id,
  label,
  n,
  max,
  children,
}: {
  id: string;
  label: string;
  n: number;
  max: number;
  children: ReactNode;
}) {
  return (
    <div className="space-y-2">
      <div className="flex items-baseline justify-between">
        <Label htmlFor={id}>{label}</Label>
        <small>
          <Counter n={n} max={max} />
        </small>
      </div>
      {children}
    </div>
  );
}

function PlatformPicker({
  selected,
  onToggle,
}: {
  selected: Platform[];
  onToggle: (id: Platform, on: boolean) => void;
}) {
  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-medium">Platforms</legend>
      <div className="flex gap-6">
        {PLATFORMS.map((p) => (
          <label key={p.id} className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              className="size-4 accent-primary"
              checked={selected.includes(p.id)}
              onChange={(e) => onToggle(p.id, e.target.checked)}
            />
            {p.name}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

export function Upload({ onQueued, onOpenSlots }: { onQueued: () => void; onOpenSlots: () => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [title, setTitle] = useState("");
  const [caption, setCaption] = useState("");
  const [platforms, setPlatforms] = useState<Platform[]>([]);
  const [when, setWhen] = useState("");
  const [problems, setProblems] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const slotOptions = useSlotOptions();
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
      setProblems([err instanceof Error ? err.message : "Something went wrong."]);
      // Someone else may have taken the slot, so look again and offer the first free one.
      if (err instanceof ApiError && err.status === 409) await slotOptions.refresh();
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
        onOpenSlots={onOpenSlots}
      />
      {problems.length > 0 && (
        <ul role="alert" className="list-disc space-y-1 pl-5 text-sm text-destructive">
          {problems.map((m) => (
            <li key={m}>{m}</li>
          ))}
        </ul>
      )}
      <Button type="submit" disabled={busy} className="w-full sm:w-auto">
        {busy ? "Queueing…" : "Queue post"}
      </Button>
    </form>
  );
}
