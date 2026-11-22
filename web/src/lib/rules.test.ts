import { describe, expect, it } from "vitest";
import {
  MAX_CAPTION_CHARS,
  MAX_TITLE_CHARS,
  MAX_UPLOAD_BYTES,
  charCount,
  formatSlot,
  localInputToUnix,
  unixToLocalInput,
  validateFields,
  validateForm,
} from "@/lib/rules";

const mp4 = (name = "reel.mp4", size = 1000) => {
  const f = new File(["x"], name, { type: "video/mp4" });
  Object.defineProperty(f, "size", { value: size });
  return f;
};

const NOW = 1_800_000_000;
const ok = {
  file: mp4(),
  title: "Title",
  caption: "",
  platforms: ["instagram" as const],
  slotAt: NOW + 3600,
  now: NOW,
};

describe("formatSlot", () => {
  it("formats local time as dd/mm/yyyy HH:mm with zero padding", () => {
    const secs = new Date(2026, 2, 5, 7, 4).getTime() / 1000;
    expect(formatSlot(secs)).toBe("05/03/2026 07:04");
  });
});

describe("localInputToUnix", () => {
  it("reads a datetime-local value as local time", () => {
    expect(localInputToUnix("2026-03-05T07:04")).toBe(new Date(2026, 2, 5, 7, 4).getTime() / 1000);
  });
  it("returns null for an empty or invalid value", () => {
    expect(localInputToUnix("")).toBeNull();
    expect(localInputToUnix("nope")).toBeNull();
  });
});

describe("charCount", () => {
  it("counts Unicode characters like the server, not UTF-16 units", () => {
    expect(charCount("a😀b")).toBe(3);
  });
});

describe("validateForm", () => {
  it("accepts a valid form", () => {
    expect(validateForm(ok)).toEqual([]);
  });
  it("requires an MP4 file", () => {
    expect(validateForm({ ...ok, file: null })).toContain("Choose an MP4 video.");
    expect(validateForm({ ...ok, file: mp4("reel.mov") })).toContain("The video must be an MP4 file.");
  });
  it("rejects an empty or oversized file", () => {
    expect(validateForm({ ...ok, file: mp4("a.mp4", 0) })).toContain("The video file is empty.");
    expect(validateForm({ ...ok, file: mp4("a.mp4", MAX_UPLOAD_BYTES) })).toEqual([]);
    expect(validateForm({ ...ok, file: mp4("a.mp4", MAX_UPLOAD_BYTES + 1) })).toContain(
      "The video is too large (100 MB limit).",
    );
  });
  it("rejects file names the server rejects", () => {
    const bad = ["..", "a\u0000b.mp4", "a\nb.mp4", "a\u007f.mp4", "a\u0085.mp4", "a/b.mp4", "a\\b.mp4", "x".repeat(252) + ".mp4"];
    for (const name of bad) {
      expect(validateForm({ ...ok, file: mp4(name) }), JSON.stringify(name)).toContain(
        "The file name is not allowed.",
      );
    }
    expect(validateForm({ ...ok, file: mp4("my..reel.mp4") })).toEqual([]);
  });
  it("requires a title within the limit", () => {
    expect(validateForm({ ...ok, title: "   " })).toContain("Title is required.");
    expect(validateForm({ ...ok, title: "t".repeat(MAX_TITLE_CHARS) })).toEqual([]);
    expect(validateForm({ ...ok, title: "t".repeat(MAX_TITLE_CHARS + 1) })).toContain(
      "Title must be 100 characters or fewer.",
    );
  });
  it("limits the caption", () => {
    expect(validateForm({ ...ok, caption: "c".repeat(MAX_CAPTION_CHARS) })).toEqual([]);
    expect(validateForm({ ...ok, caption: "c".repeat(MAX_CAPTION_CHARS + 1) })).toContain(
      "Caption must be 2,200 characters or fewer.",
    );
  });
  it("requires a platform", () => {
    expect(validateForm({ ...ok, platforms: [] })).toContain("Choose at least one platform.");
  });
  it("requires a future slot in range", () => {
    expect(validateForm({ ...ok, slotAt: null })).toContain("Choose when to publish.");
    expect(validateForm({ ...ok, slotAt: NOW - 1 })).toContain("The time must be in the future.");
    expect(validateForm({ ...ok, slotAt: 4_102_444_801 })).toContain("The time is out of range.");
  });
});

describe("unixToLocalInput", () => {
  it("gives the datetime-local value for local time and round-trips", () => {
    const secs = new Date(2099, 2, 5, 7, 4).getTime() / 1000;
    expect(unixToLocalInput(secs)).toBe("2099-03-05T07:04");
    expect(localInputToUnix(unixToLocalInput(secs))).toBe(secs);
  });
});

describe("validateFields", () => {
  const fields = { title: "T", caption: "", platforms: ["youtube" as const], slotAt: NOW + 60, now: NOW };

  it("accepts good edits and has no file rule", () => {
    expect(validateFields(fields)).toEqual([]);
  });

  it("reports the same problems as the upload form, without a file message", () => {
    const bad = { ...fields, title: " ", platforms: [], slotAt: NOW - 1 };
    expect(validateFields(bad)).toEqual([
      "Title is required.",
      "Choose at least one platform.",
      "The time must be in the future.",
    ]);
    expect(validateForm({ ...bad, file: null })[0]).toBe("Choose an MP4 video.");
  });
});
