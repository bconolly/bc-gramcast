import { useCallback, useEffect, useRef, useState } from "react";
import { getNextSlots, getPattern } from "@/lib/api";

const SLOT_COUNT = 5;

export type SlotOptions =
  | { status: "loading" }
  /** No slots to offer: none set up, all taken, or the lookup failed. */
  | { status: "none" }
  | { status: "ready"; slots: number[]; offset: number };

/**
 * The next free pattern slots and the one the owner has picked. `choice` is a slot's
 * Unix seconds, or null for "Pick another time".
 *
 * `refresh(keepOther)` looks again; unless `keepOther`, the first slot is preselected.
 * It resolves to the new choice, or undefined when a newer lookup (or unmounting)
 * made this one stale. The first lookup keeps "Pick another time" when `typed` says
 * the owner is already typing a time (focused or has typed something), so nothing
 * is chosen for them mid-entry.
 */
export function useSlotOptions(typed: boolean) {
  const [options, setOptions] = useState<SlotOptions>({ status: "loading" });
  const [choice, setChoice] = useState<number | null>(null);
  const live = useRef(true);
  // Counts lookups so an older one finishing late cannot overwrite a newer one.
  const latest = useRef(0);
  const typedRef = useRef(typed);
  useEffect(() => {
    typedRef.current = typed;
  }, [typed]);

  const refresh = useCallback(async (keepOther?: boolean): Promise<number | null | undefined> => {
    const id = ++latest.current;
    let next: SlotOptions;
    try {
      const [pattern, slots] = await Promise.all([getPattern(), getNextSlots(SLOT_COUNT)]);
      next =
        slots.length > 0
          ? { status: "ready", slots, offset: pattern.utc_offset_minutes }
          : { status: "none" };
    } catch {
      next = { status: "none" };
    }
    if (!live.current || id !== latest.current) return undefined;
    const keep = keepOther ?? typedRef.current;
    const picked = next.status === "ready" && !keep ? (next.slots[0] ?? null) : null;
    setOptions(next);
    setChoice(picked);
    return picked;
  }, []);

  /**
   * After a taken-slot error: look again. A chosen "Pick another time" stays as it was.
   * True when a chosen slot was moved to the first free one.
   */
  const refreshAfterTaken = useCallback(async (): Promise<boolean> => {
    const hadSlot = options.status === "ready" && choice !== null;
    const picked = await refresh(!hadSlot);
    return hadSlot && typeof picked === "number";
  }, [options, choice, refresh]);

  useEffect(() => {
    live.current = true;
    void refresh();
    return () => {
      live.current = false;
    };
  }, [refresh]);

  return { options, choice, setChoice, refresh, refreshAfterTaken };
}
