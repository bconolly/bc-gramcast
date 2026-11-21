import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { formatSlotInOffset, offsetName } from "@/lib/slots";
import type { SlotOptions } from "@/screens/useSlotOptions";

interface Props {
  options: SlotOptions;
  /** A listed slot's Unix seconds, or null for "Pick another time". */
  choice: number | null;
  onChoice: (slot: number | null) => void;
  when: string;
  onWhen: (value: string) => void;
  /** True while the date input has focus, so slots arriving do not choose for the owner. */
  onEditing: (editing: boolean) => void;
  disabled: boolean;
  onOpenSlots: () => void;
}

function WhenInput(props: {
  id: string;
  label: string;
  when: string;
  onWhen: (v: string) => void;
  onEditing: (editing: boolean) => void;
}) {
  return (
    <div className="space-y-2">
      <Label htmlFor={props.id}>{props.label}</Label>
      <Input
        id={props.id}
        type="datetime-local"
        className="min-h-11"
        value={props.when}
        onChange={(e) => props.onWhen(e.target.value)}
        onFocus={() => props.onEditing(true)}
        onBlur={() => props.onEditing(false)}
      />
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

export function WhenField({ options, choice, onChoice, when, onWhen, onEditing, disabled, onOpenSlots }: Props) {
  const ready = options.status === "ready";
  // One fieldset in every state, with the date input in a fixed place, so the input
  // is never remounted (and never loses focus or a half-typed value) when slots arrive.
  return (
    <fieldset disabled={disabled} className={ready ? "space-y-1" : "space-y-2"}>
      {options.status === "ready" && (
        <>
          <legend className="mb-1 text-sm font-medium">When ({offsetName(options.offset)})</legend>
          {options.slots.map((slot) => (
            <Radio
              key={slot}
              label={formatSlotInOffset(slot, options.offset)}
              checked={choice === slot}
              onSelect={() => onChoice(slot)}
            />
          ))}
          <Radio label="Pick another time" checked={choice === null} onSelect={() => onChoice(null)} />
        </>
      )}
      {(!ready || choice === null) && (
        <WhenInput
          id={ready ? "when-other" : "when"}
          label={ready ? "Date and time" : "When"}
          when={when}
          onWhen={onWhen}
          onEditing={onEditing}
        />
      )}
      {options.status === "none" && (
        <Button type="button" variant="ghost" className="text-primary min-h-11 px-0 underline" onClick={onOpenSlots}>
          Set your weekly slots
        </Button>
      )}
    </fieldset>
  );
}
