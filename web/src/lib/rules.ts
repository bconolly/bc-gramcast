// Client-side checks that mirror crates/domain so mistakes show up before an
// upload starts. The server stays the authority and re-checks everything.

import type { Platform } from "@/lib/api";

export const MAX_TITLE_CHARS = 100;
export const MAX_CAPTION_CHARS = 2200;
export const MAX_UPLOAD_BYTES = 100 * 1024 * 1024;
export const MAX_FILE_NAME_BYTES = 255;
export const MAX_SLOT_SECS = 4_102_444_800;

/** Counts Unicode characters, as the server does (not UTF-16 units). */
export function charCount(text: string): number {
  return [...text].length;
}

const pad = (n: number) => String(n).padStart(2, "0");

/** Unix seconds to dd/mm/yyyy HH:mm in the browser's local time. */
export function formatSlot(unixSecs: number): string {
  const d = new Date(unixSecs * 1000);
  return `${pad(d.getDate())}/${pad(d.getMonth() + 1)}/${d.getFullYear()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** A datetime-local value (local time, no zone) to Unix seconds, or null. */
export function localInputToUnix(value: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(value)) return null;
  const ms = new Date(value).getTime();
  return Number.isNaN(ms) ? null : Math.floor(ms / 1000);
}

/** Unix seconds to a datetime-local value (local time, to the minute). */
export function unixToLocalInput(unixSecs: number): string {
  const d = new Date(unixSecs * 1000);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export interface FieldValues {
  title: string;
  caption: string;
  platforms: Platform[];
  slotAt: number | null;
  now: number;
}

export interface FormValues extends FieldValues {
  file: File | null;
}

/** Mirrors VideoUpload::new in crates/domain/src/media.rs (\p{Cc} is Rust's char::is_control). */
function badFileName(name: string): boolean {
  return (
    new TextEncoder().encode(name).length > MAX_FILE_NAME_BYTES ||
    /[/\\]/.test(name) ||
    name === ".." ||
    /\p{Cc}/u.test(name)
  );
}

function fileProblem(file: File | null): string | null {
  if (!file) return "Choose an MP4 video.";
  if (badFileName(file.name)) return "The file name is not allowed.";
  if (!file.name.toLowerCase().endsWith(".mp4")) return "The video must be an MP4 file.";
  if (file.size === 0) return "The video file is empty.";
  if (file.size > MAX_UPLOAD_BYTES) return "The video is too large (100 MB limit).";
  return null;
}

function slotProblem(slotAt: number | null, now: number): string | null {
  if (slotAt === null) return "Choose when to publish.";
  if (slotAt < 0 || slotAt > MAX_SLOT_SECS) return "The time is out of range.";
  if (slotAt < now) return "The time must be in the future.";
  return null;
}

/** The title, caption, platform and time rules, shared by new and edited posts. */
export function validateFields(v: FieldValues): string[] {
  const problems: (string | null)[] = [];
  if (v.title.trim() === "") problems.push("Title is required.");
  else if (charCount(v.title) > MAX_TITLE_CHARS) {
    problems.push(`Title must be ${MAX_TITLE_CHARS} characters or fewer.`);
  }
  if (charCount(v.caption) > MAX_CAPTION_CHARS) {
    problems.push("Caption must be 2,200 characters or fewer.");
  }
  if (v.platforms.length === 0) problems.push("Choose at least one platform.");
  problems.push(slotProblem(v.slotAt, v.now));
  return problems.filter((p): p is string => p !== null);
}

/** Returns every problem as a message, or an empty list when the form is fine. */
export function validateForm(v: FormValues): string[] {
  const file = fileProblem(v.file);
  return file === null ? validateFields(v) : [file, ...validateFields(v)];
}
