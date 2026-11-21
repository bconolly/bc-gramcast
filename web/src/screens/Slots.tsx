import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { getPattern, savePattern, type PatternJson, type PatternRule, type WeekdayId } from "@/lib/api";
import {
  MAX_RULES,
  UTC_OFFSET_OPTIONS,
  WEEKDAYS,
  offsetLabel,
  sortRules,
  weekdayName,
} from "@/lib/slots";

const messageOf = (e: unknown) => (e instanceof Error ? e.message : "Something went wrong.");

function RuleList({ rules, onRemove }: { rules: PatternRule[]; onRemove: (r: PatternRule) => void }) {
  if (rules.length === 0) return <p className="text-muted-foreground text-sm">No weekly slots yet.</p>;
  return (
    <ul className="divide-border border-border divide-y rounded-md border">
      {rules.map((r) => {
        const text = `${weekdayName(r.weekday)} ${r.time}`;
        return (
          <li key={`${r.weekday}@${r.time}`} className="flex items-center justify-between gap-3 py-1 pr-1 pl-3">
            <span className="tabular-nums">{text}</span>
            <Button type="button" variant="ghost" aria-label={`Remove ${text}`} onClick={() => onRemove(r)}>
              Remove
            </Button>
          </li>
        );
      })}
    </ul>
  );
}

function AddRule({ rules, onAdd }: { rules: PatternRule[]; onAdd: (r: PatternRule) => void }) {
  const [weekday, setWeekday] = useState<WeekdayId>("mon");
  const [time, setTime] = useState("");
  const [problem, setProblem] = useState("");
  const full = rules.length >= MAX_RULES;

  function add() {
    if (time === "") return setProblem("Choose a time.");
    if (rules.some((r) => r.weekday === weekday && r.time === time)) {
      return setProblem("That slot is already in your list.");
    }
    setProblem("");
    onAdd({ weekday, time });
  }

  return (
    <fieldset className="space-y-3">
      <legend className="text-sm font-medium">Add a slot</legend>
      <div className="grid gap-3 sm:grid-cols-[1fr_1fr_auto] sm:items-end">
        <div className="space-y-2">
          <Label htmlFor="slot-weekday">Weekday</Label>
          <Select id="slot-weekday" value={weekday} onChange={(e) => setWeekday(e.target.value as WeekdayId)}>
            {WEEKDAYS.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name}
              </option>
            ))}
          </Select>
        </div>
        <div className="space-y-2">
          <Label htmlFor="slot-time">Time</Label>
          <Input id="slot-time" type="time" className="min-h-11" value={time} onChange={(e) => setTime(e.target.value)} />
        </div>
        <Button type="button" className="min-h-11" disabled={full} onClick={add}>
          Add slot
        </Button>
      </div>
      {full && (
        <p className="text-muted-foreground text-xs">
          {MAX_RULES} of {MAX_RULES} slots. Remove one to add another.
        </p>
      )}
      {problem && (
        <p role="alert" className="text-destructive text-sm">
          {problem}
        </p>
      )}
    </fieldset>
  );
}

function OffsetSelect({ value, onChange }: { value: number; onChange: (m: number) => void }) {
  const known = UTC_OFFSET_OPTIONS.some((o) => o.minutes === value);
  return (
    <div className="space-y-2">
      <Label htmlFor="slot-offset">UTC offset</Label>
      <Select id="slot-offset" value={String(value)} onChange={(e) => onChange(Number(e.target.value))}>
        {!known && <option value={value}>{offsetLabel(value)}</option>}
        {UTC_OFFSET_OPTIONS.map((o) => (
          <option key={o.minutes} value={o.minutes}>
            {o.label}
          </option>
        ))}
      </Select>
      <p className="text-muted-foreground text-xs">The offset is fixed. Change it when the clocks change.</p>
    </div>
  );
}

type Load = { kind: "loading" } | { kind: "error"; message: string } | { kind: "ready" };
type Outcome = { kind: "saved" } | { kind: "error"; message: string } | null;

export function Slots() {
  const [load, setLoad] = useState<Load>({ kind: "loading" });
  const [offset, setOffset] = useState(600);
  const [rules, setRules] = useState<PatternRule[]>([]);
  const [outcome, setOutcome] = useState<Outcome>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    getPattern().then(
      (p) => {
        if (!live) return;
        setOffset(p.utc_offset_minutes);
        setRules(sortRules(p.rules));
        setLoad({ kind: "ready" });
      },
      (e: unknown) => live && setLoad({ kind: "error", message: messageOf(e) }),
    );
    return () => {
      live = false;
    };
  }, []);

  function edit(next: Partial<PatternJson>) {
    setOutcome(null);
    if (next.utc_offset_minutes !== undefined) setOffset(next.utc_offset_minutes);
    if (next.rules) setRules(sortRules(next.rules));
  }

  async function save() {
    setBusy(true);
    try {
      const saved = await savePattern({ utc_offset_minutes: offset, rules });
      setOffset(saved.utc_offset_minutes);
      setRules(sortRules(saved.rules));
      setOutcome({ kind: "saved" });
    } catch (e) {
      setOutcome({ kind: "error", message: messageOf(e) });
    }
    setBusy(false);
  }

  if (load.kind === "loading") return <p className="text-muted-foreground">Loading your slots…</p>;
  if (load.kind === "error") {
    return (
      <p role="alert" className="text-destructive">
        {load.message}
      </p>
    );
  }
  return (
    <div className="space-y-6">
      <section className="space-y-3" aria-labelledby="slots-heading">
        <h2 id="slots-heading" className="text-base font-semibold">
          Weekly slots
        </h2>
        <RuleList rules={rules} onRemove={(r) => edit({ rules: rules.filter((x) => x !== r) })} />
      </section>
      <AddRule rules={rules} onAdd={(r) => edit({ rules: [...rules, r] })} />
      <OffsetSelect value={offset} onChange={(m) => edit({ utc_offset_minutes: m })} />
      <div className="flex min-h-11 items-center gap-4">
        <Button type="button" className="min-h-11" disabled={busy} onClick={save}>
          {busy ? "Saving…" : "Save"}
        </Button>
        {outcome?.kind === "saved" && <p role="status" className="text-sm">Saved.</p>}
        {outcome?.kind === "error" && (
          <p role="alert" className="text-destructive text-sm">
            {outcome.message}
          </p>
        )}
      </div>
    </div>
  );
}
