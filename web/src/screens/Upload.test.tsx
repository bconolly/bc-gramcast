import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Upload } from "@/screens/Upload";

const PUT_URL = "https://acct.r2.cloudflarestorage.com/b/videos/x.mp4?X-Amz-Signature=topsecret";

afterEach(() => vi.unstubAllGlobals());

function happyFetch() {
  const fn = vi
    .fn()
    .mockResolvedValueOnce(new Response(JSON.stringify({ video_key: "videos/x.mp4", put_url: PUT_URL })))
    .mockResolvedValueOnce(new Response(""))
    .mockResolvedValueOnce(new Response("{}", { status: 201 }));
  vi.stubGlobal("fetch", fn);
  return fn;
}

async function fillValid(user: ReturnType<typeof userEvent.setup>) {
  await user.upload(
    screen.getByLabelText("Video (MP4)"),
    new File(["abc"], "reel.mp4", { type: "video/mp4" }),
  );
  await user.type(screen.getByLabelText("Title"), "Sunday reel");
  await user.type(screen.getByLabelText("Caption"), "Hello");
  await user.click(screen.getByLabelText("Instagram"));
  fireEvent.change(screen.getByLabelText("When"), { target: { value: "2099-03-05T07:04" } });
}

describe("Upload", () => {
  it("shows character counts against the limits", async () => {
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} />);
    expect(screen.getByText("0 / 100")).toBeInTheDocument();
    expect(screen.getByText("0 / 2,200")).toBeInTheDocument();
    await user.type(screen.getByLabelText("Title"), "abc");
    await user.type(screen.getByLabelText("Caption"), "hello");
    expect(screen.getByText("3 / 100")).toBeInTheDocument();
    expect(screen.getByText("5 / 2,200")).toBeInTheDocument();
  });

  it("offers Instagram and YouTube checkboxes", () => {
    render(<Upload onQueued={vi.fn()} />);
    expect(screen.getByRole("checkbox", { name: "Instagram" })).not.toBeChecked();
    expect(screen.getByRole("checkbox", { name: "YouTube" })).not.toBeChecked();
  });

  it("blocks an invalid form and lists the problems without calling the API", async () => {
    const fn = vi.fn();
    vi.stubGlobal("fetch", fn);
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Choose an MP4 video.");
    expect(alert).toHaveTextContent("Title is required.");
    expect(alert).toHaveTextContent("Choose at least one platform.");
    expect(alert).toHaveTextContent("Choose when to publish.");
    expect(fn).not.toHaveBeenCalled();
  });

  it("uploads, PUTs, queues, then reports success", async () => {
    const fn = happyFetch();
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(fn.mock.calls.map((c) => (c as [string])[0])).toEqual(["/api/uploads", PUT_URL, "/api/posts"]);
    const body = JSON.parse((fn.mock.calls[2] as [string, RequestInit])[1].body as string);
    expect(body).toMatchObject({
      video_key: "videos/x.mp4",
      title: "Sunday reel",
      caption: "Hello",
      platforms: ["instagram"],
      slot_at: new Date(2099, 2, 5, 7, 4).getTime() / 1000,
    });
  });

  it("shows the server's message when queueing fails, and keeps the form", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(new Response(JSON.stringify({ video_key: "videos/x.mp4", put_url: PUT_URL })))
        .mockResolvedValueOnce(new Response(""))
        .mockResolvedValueOnce(new Response("that slot is already taken", { status: 409 })),
    );
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("that slot is already taken");
    expect(onQueued).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Title")).toHaveValue("Sunday reel");
  });

  it("never puts the upload link in the page", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValueOnce(new Response(JSON.stringify({ video_key: "videos/x.mp4", put_url: PUT_URL })))
        .mockResolvedValueOnce(new Response("denied", { status: 403 })),
    );
    const user = userEvent.setup();
    const { container } = render(<Upload onQueued={vi.fn()} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The video could not be uploaded. Please try again.",
    );
    expect(container.innerHTML).not.toContain("topsecret");
    expect(container.innerHTML).not.toContain("r2.cloudflarestorage");
  });

  it("disables the button while submitting so a double click can't queue twice", async () => {
    let release: (r: Response) => void = () => {};
    const pending = new Promise<Response>((r) => (release = r));
    const fn = vi.fn().mockReturnValueOnce(pending);
    vi.stubGlobal("fetch", fn);
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    expect(screen.getByRole("button", { name: "Queueing…" })).toBeDisabled();
    release(new Response("nope", { status: 500 }));
    await screen.findByRole("alert");
  });
});
