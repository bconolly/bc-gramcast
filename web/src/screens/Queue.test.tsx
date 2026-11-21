import { render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Queue } from "@/screens/Queue";

afterEach(() => vi.unstubAllGlobals());

const slot = (y: number, mo: number, d: number, h: number, mi: number) =>
  new Date(y, mo - 1, d, h, mi).getTime() / 1000;

function stub(res: Response) {
  const fn = vi.fn().mockResolvedValue(res);
  vi.stubGlobal("fetch", fn);
  return fn;
}

describe("Queue", () => {
  it("lists upcoming posts with title, platforms, local slot time and status", async () => {
    const fn = stub(
      new Response(
        JSON.stringify([
          {
            id: "a",
            video_key: "videos/a.mp4",
            title: "Sunday reel",
            caption: "Hi",
            platforms: ["instagram", "youtube"],
            slot_at: slot(2099, 3, 5, 7, 4),
            status: "ready",
          },
          {
            id: "b",
            video_key: "videos/b.mp4",
            title: "No caption yet",
            caption: "",
            platforms: ["youtube"],
            slot_at: slot(2099, 12, 31, 23, 59),
            status: "needs_caption",
          },
        ]),
      ),
    );
    render(<Queue />);
    const items = await screen.findAllByRole("listitem");
    expect(items).toHaveLength(2);
    const first = within(items[0]!);
    expect(first.getByText("Sunday reel")).toBeInTheDocument();
    expect(first.getByText("Instagram")).toBeInTheDocument();
    expect(first.getByText("YouTube")).toBeInTheDocument();
    expect(first.getByText("05/03/2099 07:04")).toBeInTheDocument();
    expect(first.getByText("Ready")).toBeInTheDocument();
    const second = within(items[1]!);
    expect(second.getByText("31/12/2099 23:59")).toBeInTheDocument();
    expect(second.getByText("Needs caption")).toBeInTheDocument();
    expect(fn.mock.calls[0]?.[0]).toBe("/api/queue");
  });

  it("says so when nothing is queued", async () => {
    stub(new Response("[]"));
    render(<Queue />);
    expect(await screen.findByText("Nothing queued yet.")).toBeInTheDocument();
  });

  it("shows the server's message when loading fails", async () => {
    stub(new Response("storage error", { status: 500 }));
    render(<Queue />);
    expect(await screen.findByRole("alert")).toHaveTextContent("storage error");
  });

  it("shows a loading state first", () => {
    stub(new Response("[]"));
    render(<Queue />);
    expect(screen.getByText("Loading the queue…")).toBeInTheDocument();
  });
});
