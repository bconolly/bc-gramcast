import type { ReactNode } from "react";
import { Label } from "@/components/ui/label";
import type { Platform } from "@/lib/api";

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

export function CountedField({
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

export function PlatformPicker({
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

export function Problems({ problems }: { problems: string[] }) {
  if (problems.length === 0) return null;
  return (
    <ul role="alert" className="list-disc space-y-1 pl-5 text-sm text-destructive">
      {problems.map((m) => (
        <li key={m}>{m}</li>
      ))}
    </ul>
  );
}
