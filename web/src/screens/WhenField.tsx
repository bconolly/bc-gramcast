import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { formatSlotInOffset } from "@/lib/slots";
import type { SlotOptions } from "@/screens/useSlotOptions";

interface Props {
  options: SlotOptions;
  /** A listed slot's Unix seconds, or null for "Pick another time". */
  choice: number | null;
  onChoice: (slot: number | null) => void;
  when: string;
  onWhen: (value: string) => void;
  onOpenSlots: () => void;
}

function WhenInput({ id, label, when, onWhen }: { id: string; label: string; when: string; onWhen: (v: string) => void }) {
  return (
    <div className="space-y-2">
      <Label htmlFor={id}>{label}</Label>
      <Input id={id} type="datetime-local" className="min-h-11" value={when} onChange={(e) => onWhen(e.target.value)} />
      <p className="text-muted-foreground text-xs">In your local time.</p>
    </div>
  );
}

function Radio({ label, checked, onSelect }: { label: string; checked: boolean; onSelect: () => void }) {
  return (
    <label className="flex min-h-11 items-center gap-3 text-sm">
      <input type="radio" name="when" className="accent-primary size-5" checked={checked} onChange={onSelect} />
      {label}
    </label>
  );
}

export function WhenField({ options, choice, onChoice, when, onWhen, onOpenSlots }: Props) {
  if (options.status === "loading") {
    return <p className="text-muted-foreground text-sm">Loading your slots…</p>;
  }
  if (options.status === "none") {
    return (
      <div className="space-y-2">
        <WhenInput id="when" label="When" when={when} onWhen={onWhen} />
        <Button type="button" variant="ghost" className="text-primary min-h-11 px-0 underline" onClick={onOpenSlots}>
          Set your weekly slots
        </Button>
      </div>
    );
  }
  return (
    <fieldset className="space-y-1">
      <legend className="mb-1 text-sm font-medium">When</legend>
      {options.slots.map((slot) => (
        <Radio
          key={slot}
          label={formatSlotInOffset(slot, options.offset)}
          checked={choice === slot}
          onSelect={() => onChoice(slot)}
        />
      ))}
      <Radio label="Pick another time" checked={choice === null} onSelect={() => onChoice(null)} />
      {choice === null && <WhenInput id="when-other" label="Date and time" when={when} onWhen={onWhen} />}
    </fieldset>
  );
}
