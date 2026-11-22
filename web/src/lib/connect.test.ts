import { describe, expect, it } from "vitest";
import { CONNECT_ERROR_MESSAGES, GENERIC_CONNECT_ERROR, readConnectResult, YOUTUBE_CONNECT_PATH } from "@/lib/connect";

describe("readConnectResult", () => {
  it("reads ?connected=1 as a success", () => {
    expect(readConnectResult("?connected=1")).toEqual({ connected: true, errorMessage: null });
  });

  it("reads nothing from an empty or unrelated query", () => {
    for (const q of ["", "?", "?x=1", "?connected=0", "?connected=yes"]) {
      expect(readConnectResult(q)).toEqual({ connected: false, errorMessage: null });
    }
  });

  it("maps each fixed error code to its own message", () => {
    const codes = ["access_denied", "bad_state", "exchange_failed", "server_error", "try_again"];
    expect(Object.keys(CONNECT_ERROR_MESSAGES).sort()).toEqual(codes.sort());
    for (const code of codes) {
      expect(readConnectResult(`?connect_error=${code}`).errorMessage).toBe(CONNECT_ERROR_MESSAGES[code]);
    }
    const all = codes.map((c) => CONNECT_ERROR_MESSAGES[c]);
    expect(new Set(all).size).toBe(codes.length);
  });

  it("tells the owner to press Connect YouTube again for try_again", () => {
    expect(readConnectResult("?connect_error=try_again").errorMessage).toBe("Please press Connect YouTube again.");
  });

  it("never echoes an unknown code", () => {
    const r = readConnectResult("?connect_error=%3Cb%3Eoops%3C%2Fb%3E");
    expect(r.errorMessage).toBe(GENERIC_CONNECT_ERROR);
    expect(r.errorMessage).not.toContain("oops");
    // Inherited object keys are not codes either.
    expect(readConnectResult("?connect_error=constructor").errorMessage).toBe(GENERIC_CONNECT_ERROR);
    expect(readConnectResult("?connect_error=").errorMessage).toBe(GENERIC_CONNECT_ERROR);
  });

  it("lets an error win over a success", () => {
    const r = readConnectResult("?connected=1&connect_error=bad_state");
    expect(r.connected).toBe(false);
    expect(r.errorMessage).toBe(CONNECT_ERROR_MESSAGES.bad_state);
  });

  it("points at the start route", () => {
    expect(YOUTUBE_CONNECT_PATH).toBe("/api/connect/youtube/start");
  });
});
