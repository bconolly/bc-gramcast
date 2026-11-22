import { describe, expect, it } from "vitest";
import {
  EDIT_LOCK_SECS,
  FAILURE_MESSAGES,
  GENERIC_FAILURE,
  TOO_CLOSE_MESSAGE,
  failureMessage,
  isChangeable,
  isLocked,
  youtubeShortsUrl,
  type PublishState,
} from "@/lib/publish";

describe("youtubeShortsUrl", () => {
  it("builds the link from the fixed template and a validated id only", () => {
    expect(youtubeShortsUrl("dQw4w9WgXcQ")).toBe("https://www.youtube.com/shorts/dQw4w9WgXcQ");
    expect(youtubeShortsUrl("a_b-C9")).toBe("https://www.youtube.com/shorts/a_b-C9");
    expect(youtubeShortsUrl("a".repeat(32))).toBe(`https://www.youtube.com/shorts/${"a".repeat(32)}`);
  });

  it("refuses anything that is not 1 to 32 of letters, digits, underscore and hyphen", () => {
    const bad = [
      "",
      "a".repeat(33),
      "a b",
      "a/b",
      "../x",
      "a?x=1",
      "a#b",
      "a@evil.com",
      "https://evil.example/x",
      "javascript:alert(1)",
      "é",
      "a\nb",
      undefined,
      null,
    ];
    for (const id of bad) expect(youtubeShortsUrl(id), String(id)).toBeNull();
  });
});

describe("failureMessage", () => {
  it("has a fixed, different message for each reason code", () => {
    const codes = ["auth", "rate_limited", "rejected", "transient", "stuck_publishing", "unknown"];
    expect(Object.keys(FAILURE_MESSAGES).sort()).toEqual([...codes].sort());
    const texts = codes.map((c) => failureMessage(c));
    expect(new Set(texts).size).toBe(codes.length);
    for (const t of texts) expect(t.length).toBeGreaterThan(20);
  });

  it("sends the owner to YouTube Studio where a video may exist", () => {
    expect(failureMessage("unknown")).toMatch(/YouTube Studio/);
    expect(failureMessage("stuck_publishing")).toMatch(/YouTube Studio/);
    expect(failureMessage("transient")).toMatch(/nothing was posted/i);
  });

  it("never echoes an unknown or missing code", () => {
    expect(failureMessage("<script>alert(1)</script>")).toBe(GENERIC_FAILURE);
    expect(failureMessage("toString")).toBe(GENERIC_FAILURE);
    expect(failureMessage(undefined)).toBe(GENERIC_FAILURE);
    expect(GENERIC_FAILURE).not.toContain("script");
  });
});

describe("isChangeable", () => {
  const post = (slot_at: number, state: PublishState = "queued") => ({ slot_at, publish: { state } });
  it("needs a queued post whose slot is more than the lock window away, as the Worker does", () => {
    expect(EDIT_LOCK_SECS).toBe(120);
    expect(isChangeable(post(1000 + EDIT_LOCK_SECS + 1), 1000)).toBe(true);
    expect(isChangeable(post(1000 + EDIT_LOCK_SECS), 1000)).toBe(false);
    expect(isChangeable(post(1000), 1000)).toBe(false);
    expect(isChangeable(post(5000, "publishing"), 1000)).toBe(false);
    expect(isChangeable({ slot_at: 5000 }, 1000)).toBe(true);
  });
  it("tells a post inside the window from one that has passed", () => {
    expect(isLocked(post(1000 + EDIT_LOCK_SECS), 1000)).toBe(true);
    expect(isLocked(post(1001), 1000)).toBe(true);
    expect(isLocked(post(1000), 1000)).toBe(false);
    expect(isLocked(post(5000, "failed"), 1000)).toBe(false);
    expect(isLocked(post(1000 + EDIT_LOCK_SECS + 1), 1000)).toBe(false);
  });
  it("uses the Worker's words", () => {
    expect(TOO_CLOSE_MESSAGE).toBe("Too close to its slot to change.");
  });
});
