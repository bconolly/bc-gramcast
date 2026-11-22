import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
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

const POSTS = [
  { id: "a1", video_key: "videos/a1.mp4", title: "Sunday reel", caption: "Hi", platforms: ["instagram", "youtube"], slot_at: slot(2099, 3, 5, 7, 4), status: "ready" },
  { id: "b2", video_key: "videos/b2.mp4", title: "Second post", caption: "", platforms: ["youtube"], slot_at: slot(2099, 3, 6, 7, 4), status: "needs_caption" },
];

/** Serves the queue, and answers any other call with `other` (default 204). */
function serve(other: (url: string, init?: RequestInit) => Promise<Response> | Response = () => new Response(null, { status: 204 })) {
  const fn = vi.fn(async (url: string, init?: RequestInit) =>
    url === "/api/queue" ? new Response(JSON.stringify(POSTS)) : other(url, init),
  );
  vi.stubGlobal("fetch", fn);
  return fn;
}

const rowOf = async (title: string) => (await screen.findByText(title)).closest("li") as HTMLElement;

describe("Queue delete", () => {
  it("gives each post a Delete button named after the post, 44px tall", async () => {
    serve();
    render(<Queue />);
    const row = within(await rowOf("Sunday reel"));
    expect(row.getByRole("button", { name: "Delete Sunday reel" })).toHaveClass("min-h-11");
    expect(screen.getByRole("button", { name: "Delete Second post" })).toBeInTheDocument();
  });

  it("asks in the page first, never with window.confirm, and Cancel backs out", async () => {
    const confirmSpy = vi.spyOn(window, "confirm");
    const fn = serve();
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Sunday reel" }));
    const row = within(await rowOf("Sunday reel"));
    expect(row.getByText(/cannot be undone/i)).toBeInTheDocument();
    expect(row.getByRole("button", { name: "Delete post" })).toBeInTheDocument();
    await user.click(row.getByRole("button", { name: "Cancel" }));
    expect(row.queryByRole("button", { name: "Delete post" })).not.toBeInTheDocument();
    expect(row.getByRole("button", { name: "Delete Sunday reel" })).toBeInTheDocument();
    expect(confirmSpy).not.toHaveBeenCalled();
    expect(fn.mock.calls.filter(([, init]) => init?.method === "DELETE")).toHaveLength(0);
  });

  it("deletes after confirming, shows progress, then removes only that row", async () => {
    let finish: (r: Response) => void = () => undefined;
    const fn = serve(() => new Promise<Response>((r) => (finish = r)));
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Sunday reel" }));
    await user.click(screen.getByRole("button", { name: "Delete post" }));
    expect(screen.getByRole("button", { name: "Deleting…" })).toBeDisabled();
    expect(fn).toHaveBeenCalledWith("/api/posts/a1", { method: "DELETE" });
    finish(new Response(null, { status: 204 }));
    await waitFor(() => expect(screen.queryByText("Sunday reel")).not.toBeInTheDocument());
    expect(screen.getByText("Second post")).toBeInTheDocument();
  });

  it("keeps the row and shows the server's message when delete fails", async () => {
    const [message, status] = ["storage error", 500] as const;
    serve(() => new Response(message, { status }));
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Sunday reel" }));
    await user.click(screen.getByRole("button", { name: "Delete post" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(message);
    expect(screen.getByText("Sunday reel")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Delete post" })).toBeEnabled();
  });

  it("removes the row and says so when the post was already deleted elsewhere (404)", async () => {
    serve(() => new Response("post not found", { status: 404 }));
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Sunday reel" }));
    await user.click(screen.getByRole("button", { name: "Delete post" }));
    await waitFor(() => expect(screen.queryByText("Sunday reel")).not.toBeInTheDocument());
    expect(screen.getByRole("status")).toHaveTextContent("That post was already deleted.");
    expect(screen.getByText("Second post")).toBeInTheDocument();
  });

  it("removes the row and says why when the post is no longer queued (409)", async () => {
    serve(() => new Response("post is no longer queued", { status: 409 }));
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Sunday reel" }));
    await user.click(screen.getByRole("button", { name: "Delete post" }));
    await waitFor(() => expect(screen.queryByText("Sunday reel")).not.toBeInTheDocument());
    expect(screen.getByRole("status")).toHaveTextContent("That post is no longer queued, so it was removed from the list.");
    expect(screen.getByText("Second post")).toBeInTheDocument();
  });

  it("announces the delete politely and moves focus to the next row's Edit button", async () => {
    serve();
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Sunday reel" }));
    await user.click(screen.getByRole("button", { name: "Delete post" }));
    const status = await screen.findByRole("status");
    expect(status).toHaveTextContent("Post deleted");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(screen.getByRole("button", { name: "Edit Second post" })).toHaveFocus();
  });

  it("moves focus to the previous row's Edit button when the last row is deleted", async () => {
    serve();
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Second post" }));
    await user.click(screen.getByRole("button", { name: "Delete post" }));
    await waitFor(() => expect(screen.queryByText("Second post")).not.toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Edit Sunday reel" })).toHaveFocus();
  });

  it("moves focus to the empty message when the only post is deleted", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string) =>
        url === "/api/queue" ? new Response(JSON.stringify([POSTS[0]])) : new Response(null, { status: 204 }),
      ),
    );
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Sunday reel" }));
    await user.click(screen.getByRole("button", { name: "Delete post" }));
    expect(await screen.findByText("Nothing queued yet.")).toHaveFocus();
    expect(screen.getByRole("status")).toHaveTextContent("Post deleted");
  });

  it("returns focus to the Delete button after Cancel", async () => {
    serve();
    const user = userEvent.setup();
    render(<Queue />);
    await user.click(await screen.findByRole("button", { name: "Delete Sunday reel" }));
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("button", { name: "Delete Sunday reel" })).toHaveFocus();
  });
});
