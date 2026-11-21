// Weekly slot helpers. Pure: nothing here reads the browser's timezone.

import type { WeekdayId } from "@/lib/api";

export const MAX_RULES = 14;
export const DEFAULT_OFFSET_MINUTES = 600;

/** The fixed UTC offsets the owner can pick, in minutes. */
export const UTC_OFFSET_OPTIONS: { minutes: number; label: string }[] = [
  { minutes: 480, label: "Perth UTC+8" },
  { minutes: 570, label: "Adelaide/Darwin UTC+9:30" },
  { minutes: 600, label: "Brisbane UTC+10" },
  { minutes: 660, label: "Sydney/Melbourne daylight saving UTC+11" },
  { minutes: 720, label: "Auckland UTC+12" },
  { minutes: 780, label: "Auckland daylight saving UTC+13" },
];

export const WEEKDAYS: { id: WeekdayId; name: string }[] = [
  { id: "mon", name: "Monday" },
  { id: "tue", name: "Tuesday" },
  { id: "wed", name: "Wednesday" },
  { id: "thu", name: "Thursday" },
  { id: "fri", name: "Friday" },
  { id: "sat", name: "Saturday" },
  { id: "sun", name: "Sunday" },
];

const SHORT_DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const pad = (n: number) => String(n).padStart(2, "0");

/**
 * Unix seconds shown as e.g. "Wed 26/11/2026 19:00" in a fixed UTC offset.
 * Shifts by the offset and reads the UTC getters, so the browser's zone is irrelevant.
 */
export function formatSlotInOffset(slotAt: number, offsetMinutes: number): string {
  const d = new Date((slotAt + offsetMinutes * 60) * 1000);
  const day = `${pad(d.getUTCDate())}/${pad(d.getUTCMonth() + 1)}/${d.getUTCFullYear()}`;
  return `${SHORT_DAYS[d.getUTCDay()]} ${day} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`;
}
