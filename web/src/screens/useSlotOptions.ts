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
 * Unix seconds, or null for "Pick another time". `refresh` looks again and
 * preselects the first slot.
 */
export function useSlotOptions() {
  const [options, setOptions] = useState<SlotOptions>({ status: "loading" });
  const [choice, setChoice] = useState<number | null>(null);
  const live = useRef(true);

  const refresh = useCallback(async () => {
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
    if (!live.current) return;
    setOptions(next);
    setChoice(next.status === "ready" ? (next.slots[0] ?? null) : null);
  }, []);

  useEffect(() => {
    live.current = true;
    void refresh();
    return () => {
      live.current = false;
    };
  }, [refresh]);

  return { options, choice, setChoice, refresh };
}
