import { useState, type FormEvent } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { isSlotTaken, updatePost, type Platform, type PostJson } from "@/lib/api";
import {
  MAX_CAPTION_CHARS,
  MAX_TITLE_CHARS,
  charCount,
  localInputToUnix,
  unixToLocalInput,
  validateFields,
} from "@/lib/rules";
import { CountedField, PlatformPicker, Problems } from "@/screens/PostFields";
import { WhenField } from "@/screens/WhenField";
import { useSlotOptions } from "@/screens/useSlotOptions";

interface Props {
  post: PostJson;
  onSaved: (post: PostJson) => void;
  onCancel: () => void;
  onOpenSlots: () => void;
}

/** Inline edit of the title, caption, platforms and slot. The video never changes. */
export function EditPostForm({ post, onSaved, onCancel, onOpenSlots }: Props) {
  const [title, setTitle] = useState(post.title);
  const [caption, setCaption] = useState(post.caption);
  const [platforms, setPlatforms] = useState<Platform[]>(post.platforms);
  const [when, setWhen] = useState(unixToLocalInput(post.slot_at));
  const [problems, setProblems] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  // Starts on "Pick another time" with the current slot filled in, so a save without
  // touching the time keeps it.
  const slots = useSlotOptions(true);

  async function onSubmit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    const slotAt =
      slots.options.status === "ready" && slots.choice !== null ? slots.choice : localInputToUnix(when);
    const found = validateFields({ title, caption, platforms, slotAt, now: Math.floor(Date.now() / 1000) });
    setProblems(found);
    if (found.length > 0 || slotAt === null) return;
    setBusy(true);
    try {
      onSaved(await updatePost(post.id, { title, caption, platforms, slot_at: slotAt }));
    } catch (err) {
      const message = err instanceof Error ? err.message : "Something went wrong.";
      setProblems([message]);
      if (isSlotTaken(err) && (await slots.refreshAfterTaken())) {
        setProblems([message, "We have picked the next free slot."]);
      }
      setBusy(false);
    }
  }

  const toggle = (id: Platform, on: boolean) =>
    setPlatforms((cur) => (on ? [...cur, id] : cur.filter((p) => p !== id)));

  return (
    <form aria-label={`Edit ${post.title}`} onSubmit={onSubmit} noValidate className="space-y-5 border-t pt-4">
      <CountedField id={`title-${post.id}`} label="Title" n={charCount(title)} max={MAX_TITLE_CHARS}>
        <Input id={`title-${post.id}`} value={title} onChange={(e) => setTitle(e.target.value)} />
      </CountedField>
      <CountedField id={`caption-${post.id}`} label="Caption" n={charCount(caption)} max={MAX_CAPTION_CHARS}>
        <Textarea id={`caption-${post.id}`} value={caption} onChange={(e) => setCaption(e.target.value)} />
      </CountedField>
      <PlatformPicker selected={platforms} onToggle={toggle} />
      <WhenField
        options={slots.options}
        choice={slots.choice}
        onChoice={slots.setChoice}
        when={when}
        onWhen={setWhen}
        onEditing={() => undefined}
        disabled={busy}
        onOpenSlots={onOpenSlots}
      />
      <Problems problems={problems} />
      <div className="flex flex-wrap gap-3">
        <Button type="submit" className="h-11 min-h-11" disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button type="button" variant="outline" className="h-11 min-h-11" disabled={busy} onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
