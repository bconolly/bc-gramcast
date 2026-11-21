import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { submitPost, type Platform } from "@/lib/api";
import {
  MAX_CAPTION_CHARS,
  MAX_TITLE_CHARS,
  charCount,
  localInputToUnix,
  validateForm,
} from "@/lib/rules";

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

export function Upload({ onQueued }: { onQueued: () => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [title, setTitle] = useState("");
  const [caption, setCaption] = useState("");
  const [platforms, setPlatforms] = useState<Platform[]>([]);
  const [when, setWhen] = useState("");
  const [problems, setProblems] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);

  function toggle(id: Platform, on: boolean) {
    setPlatforms((cur) => (on ? [...cur, id] : cur.filter((p) => p !== id)));
  }

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const slotAt = localInputToUnix(when);
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
      await submitPost({ file, title, caption, platforms, slot_at: slotAt });
      onQueued();
    } catch (err) {
      setProblems([err instanceof Error ? err.message : "Something went wrong."]);
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
          onChange={(e) => setFile(e.target.files?.[0] ?? null)}
        />
      </div>
      <div className="space-y-2">
        <div className="flex items-baseline justify-between">
          <Label htmlFor="title">Title</Label>
          <small>
            <Counter n={charCount(title)} max={MAX_TITLE_CHARS} />
          </small>
        </div>
        <Input id="title" value={title} onChange={(e) => setTitle(e.target.value)} />
      </div>
      <div className="space-y-2">
        <div className="flex items-baseline justify-between">
          <Label htmlFor="caption">Caption</Label>
          <small>
            <Counter n={charCount(caption)} max={MAX_CAPTION_CHARS} />
          </small>
        </div>
        <Textarea id="caption" value={caption} onChange={(e) => setCaption(e.target.value)} />
      </div>
      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Platforms</legend>
        <div className="flex gap-6">
          {PLATFORMS.map((p) => (
            <label key={p.id} className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                className="size-4 accent-primary"
                checked={platforms.includes(p.id)}
                onChange={(e) => toggle(p.id, e.target.checked)}
              />
              {p.name}
            </label>
          ))}
        </div>
      </fieldset>
      <div className="space-y-2">
        <Label htmlFor="when">When</Label>
        <Input
          id="when"
          type="datetime-local"
          value={when}
          onChange={(e) => setWhen(e.target.value)}
        />
        <p className="text-xs text-muted-foreground">In your local time.</p>
      </div>
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
