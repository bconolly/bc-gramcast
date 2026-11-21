import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  getNextSlots,
  getPattern,
  getQueue,
  putVideo,
  queuePost,
  savePattern,
  startUpload,
  submitPost,
} from "@/lib/api";

const PUT_URL = "https://acct.r2.cloudflarestorage.com/b/videos/x.mp4?X-Amz-Signature=sig";

function reply(body: string, status = 200) {
  return new Response(body, { status });
}

function mockFetch(...responses: Response[]) {
  const fn = vi.fn();
  for (const r of responses) fn.mockResolvedValueOnce(r);
  vi.stubGlobal("fetch", fn);
  return fn;
}

afterEach(() => vi.unstubAllGlobals());

describe("getQueue", () => {
  it("returns the posts from GET /api/queue", async () => {
    const posts = [{ id: "a", title: "T", platforms: ["instagram"], slot_at: 100, status: "ready" }];
    const fetchFn = mockFetch(reply(JSON.stringify(posts)));
    expect(await getQueue()).toEqual(posts);
    expect(fetchFn.mock.calls[0]?.[0]).toBe("/api/queue");
  });

  it("shows the server's fixed message on failure", async () => {
    mockFetch(reply("storage error", 500));
    await expect(getQueue()).rejects.toMatchObject({ message: "storage error", status: 500 });
  });
});

describe("startUpload", () => {
  it("POSTs JSON with the JSON content type", async () => {
    const fetchFn = mockFetch(reply(JSON.stringify({ video_key: "videos/x.mp4", put_url: PUT_URL })));
    const ticket = await startUpload({ file_name: "a.mp4", content_type: "video/mp4", size_bytes: 5 });
    expect(ticket.video_key).toBe("videos/x.mp4");
    const [url, init] = fetchFn.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/uploads");
    expect(init.method).toBe("POST");
    expect(new Headers(init.headers).get("Content-Type")).toBe("application/json");
    expect(JSON.parse(init.body as string)).toEqual({
      file_name: "a.mp4",
      content_type: "video/mp4",
      size_bytes: 5,
    });
  });

  it("surfaces a 422 message from the server", async () => {
    mockFetch(reply("file must be an MP4", 422));
    await expect(
      startUpload({ file_name: "a.mov", content_type: "video/mp4", size_bytes: 5 }),
    ).rejects.toMatchObject({ message: "file must be an MP4", status: 422 });
  });
});

describe("putVideo", () => {
  it("PUTs the file with Content-Type video/mp4", async () => {
    const fetchFn = mockFetch(reply("", 200));
    const file = new File(["abc"], "a.mp4", { type: "video/mp4" });
    await putVideo(PUT_URL, file);
    const [url, init] = fetchFn.mock.calls[0] as [string, RequestInit];
    expect(url).toBe(PUT_URL);
    expect(init.method).toBe("PUT");
    expect(new Headers(init.headers).get("Content-Type")).toBe("video/mp4");
    expect(init.body).toBe(file);
  });

  it("fails with a generic message that never contains the link", async () => {
    mockFetch(reply("<Error>signature mismatch</Error>", 403));
    const err = await putVideo(PUT_URL, new File(["a"], "a.mp4")).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).message).toBe("The video could not be uploaded. Please try again.");
    expect(JSON.stringify(err)).not.toContain("Signature");
    expect((err as ApiError).message).not.toContain("r2.cloudflarestorage");
  });

  it("hides the link when the network fails", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError(`Failed to fetch ${PUT_URL}`)));
    const err = (await putVideo(PUT_URL, new File(["a"], "a.mp4")).catch((e: unknown) => e)) as ApiError;
    expect(err.message).toBe("The video could not be uploaded. Please try again.");
    expect(String(err.cause ?? "")).not.toContain("r2.cloudflarestorage");
  });
});

describe("queuePost", () => {
  it("POSTs the post as JSON", async () => {
    const fetchFn = mockFetch(reply(JSON.stringify({ id: "p1" }), 201));
    await queuePost({
      video_key: "videos/x.mp4",
      title: "T",
      caption: "C",
      platforms: ["instagram", "youtube"],
      slot_at: 1_900_000_000,
    });
    const [url, init] = fetchFn.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/posts");
    expect(new Headers(init.headers).get("Content-Type")).toBe("application/json");
  });

  it("surfaces a 409 message", async () => {
    mockFetch(reply("that slot is already taken", 409));
    await expect(
      queuePost({ video_key: "videos/x.mp4", title: "T", caption: "", platforms: ["youtube"], slot_at: 1 }),
    ).rejects.toMatchObject({ message: "that slot is already taken", status: 409 });
  });

  it("falls back to a generic message for an odd error body", async () => {
    mockFetch(reply("<html>" + "x".repeat(500) + "</html>", 502));
    await expect(
      queuePost({ video_key: "k", title: "T", caption: "", platforms: ["youtube"], slot_at: 1 }),
    ).rejects.toMatchObject({ message: "Something went wrong (error 502). Please try again." });
  });
});

describe("submitPost", () => {
  const file = new File(["abc"], "reel.mp4", { type: "video/mp4" });
  const input = {
    file,
    title: "T",
    caption: "C",
    platforms: ["instagram" as const],
    slot_at: 1_900_000_000,
  };

  it("runs upload, PUT, then queue in order", async () => {
    const fetchFn = mockFetch(
      reply(JSON.stringify({ video_key: "videos/x.mp4", put_url: PUT_URL })),
      reply("", 200),
      reply(JSON.stringify({ id: "p1" }), 201),
    );
    await submitPost(input);
    expect(fetchFn.mock.calls.map((c) => (c as [string])[0])).toEqual([
      "/api/uploads",
      PUT_URL,
      "/api/posts",
    ]);
    const body = JSON.parse((fetchFn.mock.calls[2] as [string, RequestInit])[1].body as string);
    expect(body).toEqual({
      video_key: "videos/x.mp4",
      title: "T",
      caption: "C",
      platforms: ["instagram"],
      slot_at: 1_900_000_000,
    });
  });

  it("stops before queueing when the PUT fails", async () => {
    const fetchFn = mockFetch(
      reply(JSON.stringify({ video_key: "videos/x.mp4", put_url: PUT_URL })),
      reply("no", 403),
    );
    await expect(submitPost(input)).rejects.toBeInstanceOf(ApiError);
    expect(fetchFn).toHaveBeenCalledTimes(2);
  });
});

describe("slot pattern", () => {
  const pattern = { utc_offset_minutes: 600, rules: [{ weekday: "wed" as const, time: "19:00" }] };

  it("getPattern reads GET /api/slots/pattern", async () => {
    const fetchFn = mockFetch(reply(JSON.stringify(pattern)));
    expect(await getPattern()).toEqual(pattern);
    expect(fetchFn.mock.calls[0]?.[0]).toBe("/api/slots/pattern");
  });

  it("savePattern PUTs JSON with the JSON content type and returns the normalised pattern", async () => {
    const fetchFn = mockFetch(reply(JSON.stringify(pattern)));
    expect(await savePattern(pattern)).toEqual(pattern);
    const [url, init] = fetchFn.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/slots/pattern");
    expect(init.method).toBe("PUT");
    expect(new Headers(init.headers).get("Content-Type")).toBe("application/json");
    expect(JSON.parse(init.body as string)).toEqual(pattern);
  });

  it("savePattern shows the server's fixed message on failure", async () => {
    mockFetch(reply("too many rules", 422));
    await expect(savePattern(pattern)).rejects.toMatchObject({ message: "too many rules", status: 422 });
  });

  it("getNextSlots asks for count and returns the Unix seconds", async () => {
    const fetchFn = mockFetch(reply(JSON.stringify({ slots: [100, 200] })));
    expect(await getNextSlots(5)).toEqual([100, 200]);
    expect(fetchFn.mock.calls[0]?.[0]).toBe("/api/slots/next?count=5");
  });

  it("getNextSlots surfaces a failure", async () => {
    mockFetch(reply("count must be a whole number from 1 to 20", 422));
    await expect(getNextSlots(0)).rejects.toMatchObject({ status: 422 });
  });
});
