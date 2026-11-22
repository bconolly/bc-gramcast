import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Queue } from "@/screens/Queue";

afterEach(() => vi.unstubAllGlobals());

const slot = (y: number, mo: number, d: number, h: number, mi: number) =>
  new Date(y, mo - 1, d, h, mi).getTime() / 1000;

const POSTS = [
  { id: "a1", video_key: "videos/a1.mp4", title: "Sunday reel", caption: "Hi", platforms: ["instagram", "youtube"], slot_at: slot(2099, 3, 5, 7, 4), status: "ready" },
  { id: "b2", video_key: "videos/b2.mp4", title: "Second post", caption: "", platforms: ["youtube"], slot_at: slot(2099, 3, 8, 9, 0), status: "needs_caption" },
];
const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });

/** Serves the queue, no weekly slots, and answers PATCH with `patch`. */
function serve(
  patch: (init: RequestInit) => Promise<Response> | Response = () => json(POSTS[0]),
  nextSlots: number[][] = [[]],
) {
  let lookups = 0;
  const fn = vi.fn(async (url: string, init?: RequestInit) => {
    if (url === "/api/queue") return json(POSTS);
    if (url === "/api/slots/pattern") return json({ utc_offset_minutes: 600, rules: [] });
    if (url.startsWith("/api/slots/next")) return json({ slots: nextSlots[Math.min(lookups++, nextSlots.length - 1)] });
    return patch(init ?? {});
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

const patchCalls = (fn: ReturnType<typeof serve>) =>
  fn.mock.calls.filter(([, init]) => init?.method === "PATCH");

async function openEdit(user: ReturnType<typeof userEvent.setup>, name = "Edit Sunday reel") {
  render(<Queue />);
  await user.click(await screen.findByRole("button", { name }));
  return within(screen.getByRole("form", { name }));
}

describe("Queue edit", () => {
  it("gives each post an Edit button named after the post, 44px tall", async () => {
    serve();
    render(<Queue />);
    expect(await screen.findByRole("button", { name: "Edit Sunday reel" })).toHaveClass("min-h-11");
    expect(screen.getByRole("button", { name: "Edit Second post" })).toBeInTheDocument();
  });

  it("opens an inline form filled with the post's values", async () => {
    serve();
    const form = await openEdit(userEvent.setup());
    expect(form.getByLabelText("Title")).toHaveValue("Sunday reel");
    expect(form.getByLabelText("Caption")).toHaveValue("Hi");
    expect(form.getByLabelText("Instagram")).toBeChecked();
    expect(form.getByLabelText("YouTube")).toBeChecked();
    expect(form.getByLabelText("When")).toHaveValue("2099-03-05T07:04");
    expect(form.getByRole("button", { name: "Save" })).toBeInTheDocument();
    expect(form.getByLabelText("Instagram").closest("label")).toHaveClass("min-h-11");
  });

  it("moves focus to the title when the form opens", async () => {
    serve();
    const form = await openEdit(userEvent.setup());
    expect(form.getByLabelText("Title")).toHaveFocus();
  });

  it("returns focus to the post's Edit button after Cancel", async () => {
    serve();
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.click(form.getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("button", { name: "Edit Sunday reel" })).toHaveFocus();
  });

  it("returns focus to the post's Edit button after Save", async () => {
    serve();
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.click(form.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("button", { name: "Edit Sunday reel" })).toHaveFocus();
  });

  it("when a 409 moves a chosen slot, picks the next free one and says so", async () => {
    const [s1, s2, s3] = [Date.UTC(2099, 2, 5, 22, 30), Date.UTC(2099, 2, 8, 20, 30), Date.UTC(2099, 2, 11, 9, 0)].map((ms) => ms / 1000);
    const fn = serve(() => new Response("that slot is already taken", { status: 409 }), [[s1!, s2!], [s2!, s3!]]);
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.click(await form.findByRole("radio", { name: "Fri 06/03/2099 08:30" }));
    await user.click(form.getByRole("button", { name: "Save" }));
    const alert = await form.findByRole("alert");
    expect(alert).toHaveTextContent("that slot is already taken");
    expect(alert).toHaveTextContent("We have picked the next free slot.");
    expect(await form.findByRole("radio", { name: "Mon 09/03/2099 06:30" })).toBeChecked();
    expect(JSON.parse(patchCalls(fn)[0]![1]!.body as string).slot_at).toBe(s1);
  });

  it("Cancel closes the form without calling the server", async () => {
    const fn = serve();
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.type(form.getByLabelText("Title"), " changed");
    await user.click(form.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("form")).not.toBeInTheDocument();
    expect(screen.getByText("Sunday reel")).toBeInTheDocument();
    expect(patchCalls(fn)).toHaveLength(0);
  });

  it("saves the changes, shows progress, then shows the updated post", async () => {
    let finish: (r: Response) => void = () => undefined;
    const fn = serve(() => new Promise<Response>((r) => (finish = r)));
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.clear(form.getByLabelText("Title"));
    await user.type(form.getByLabelText("Title"), "New title");
    await user.click(form.getByLabelText("Instagram"));
    await user.click(form.getByRole("button", { name: "Save" }));
    expect(form.getByRole("button", { name: "Saving…" })).toBeDisabled();
    const [url, init] = patchCalls(fn)[0] as [string, RequestInit];
    expect(url).toBe("/api/posts/a1");
    expect(JSON.parse(init.body as string)).toEqual({
      title: "New title",
      caption: "Hi",
      platforms: ["youtube"],
      slot_at: POSTS[0]!.slot_at,
    });
    finish(json({ ...POSTS[0], title: "New title", platforms: ["youtube"] }));
    expect(await screen.findByText("New title")).toBeInTheDocument();
    expect(screen.queryByRole("form")).not.toBeInTheDocument();
    expect(screen.queryByText("Sunday reel")).not.toBeInTheDocument();
  });

  it("announces a successful save in the live region", async () => {
    serve();
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.click(form.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Post saved");
  });

  it("removes the row and says why when the post is no longer queued (409)", async () => {
    serve(() => new Response("post is no longer queued", { status: 409 }));
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.click(form.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByText("Sunday reel")).not.toBeInTheDocument());
    expect(screen.getByRole("status")).toHaveTextContent("That post is no longer queued, so it was removed from the list.");
    expect(screen.getByText("Second post")).toBeInTheDocument();
  });

  it("shows validation problems and does not call the server", async () => {
    const fn = serve();
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.clear(form.getByLabelText("Title"));
    await user.click(form.getByLabelText("Instagram"));
    await user.click(form.getByLabelText("YouTube"));
    await user.click(form.getByRole("button", { name: "Save" }));
    const alert = await form.findByRole("alert");
    expect(alert).toHaveTextContent("Title is required.");
    expect(alert).toHaveTextContent("Choose at least one platform.");
    expect(patchCalls(fn)).toHaveLength(0);
  });

  it("tells the owner when the slot was taken (409) and keeps the form open", async () => {
    serve(() => new Response("that slot is already taken", { status: 409 }));
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.click(form.getByRole("button", { name: "Save" }));
    expect(await form.findByRole("alert")).toHaveTextContent("that slot is already taken");
    expect(form.getByRole("button", { name: "Save" })).toBeEnabled();
  });

  it("shows another server error as it comes and keeps the typed values", async () => {
    serve(() => new Response("storage error", { status: 500 }));
    const user = userEvent.setup();
    const form = await openEdit(user);
    await user.type(form.getByLabelText("Title"), "!");
    await user.click(form.getByRole("button", { name: "Save" }));
    expect(await form.findByRole("alert")).toHaveTextContent("storage error");
    expect(form.getByLabelText("Title")).toHaveValue("Sunday reel!");
  });

  it("has only one post open for editing or deleting at a time", async () => {
    serve();
    const user = userEvent.setup();
    await openEdit(user);
    await user.click(screen.getByRole("button", { name: "Delete Second post" }));
    await waitFor(() => expect(screen.queryByRole("form")).not.toBeInTheDocument());
    expect(screen.getByRole("button", { name: "Delete post" })).toBeInTheDocument();
  });
});
