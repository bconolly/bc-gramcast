import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Upload } from "@/screens/Upload";

const PUT_URL = "https://acct.r2.cloudflarestorage.com/b/videos/x.mp4?X-Amz-Signature=topsecret";
// Brisbane (+10:00) slots. Labels worked out independently with Python datetime.
const SLOT_A = 4076298000; // Wed 04/03/2099 19:00
const SLOT_B = 4076433000; // Fri 06/03/2099 08:30
const SLOT_C = 4076902800; // Wed 11/03/2099 19:00

afterEach(() => vi.unstubAllGlobals());

const json = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
const ticket = () => json({ video_key: "videos/x.mp4", put_url: PUT_URL });

/**
 * Routes by URL. Each slots lookup takes the next entry of `slotReplies` (the last one
 * repeats); every other call takes the next entry of `api`, in order.
 */
function stubApi({ slotReplies = [json({ slots: [] })], api = [] as Response[], offset = 600 } = {}) {
  const slots = [...slotReplies];
  const queue = [...api];
  const fn = vi.fn<(url: string, init?: RequestInit) => Promise<Response>>((url) => {
    if (url === "/api/slots/pattern") return Promise.resolve(json({ utc_offset_minutes: offset, rules: [] }));
    if (url.startsWith("/api/slots/next")) {
      return Promise.resolve(slots.length > 1 ? slots.shift()! : slots[0]!.clone());
    }
    return Promise.resolve(queue.shift() ?? new Response("unexpected call", { status: 500 }));
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

/** The calls that are not slot lookups. */
const apiCalls = (fn: ReturnType<typeof stubApi>) =>
  fn.mock.calls.filter((c) => !String(c[0]).startsWith("/api/slots/"));

function happyFetch() {
  return stubApi({ api: [ticket(), new Response(""), new Response("{}", { status: 201 })] });
}

async function fillValid(user: ReturnType<typeof userEvent.setup>) {
  await user.upload(
    screen.getByLabelText("Video (MP4)"),
    new File(["abc"], "reel.mp4", { type: "video/mp4" }),
  );
  await user.type(screen.getByLabelText("Title"), "Sunday reel");
  await user.type(screen.getByLabelText("Caption"), "Hello");
  await user.click(screen.getByLabelText("Instagram"));
  await screen.findByLabelText("When");
  fireEvent.change(screen.getByLabelText("When"), { target: { value: "2099-03-05T07:04" } });
}

describe("Upload", () => {
  it("shows character counts against the limits", async () => {
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    expect(screen.getByText("0 / 100")).toBeInTheDocument();
    expect(screen.getByText("0 / 2,200")).toBeInTheDocument();
    await user.type(screen.getByLabelText("Title"), "abc");
    await user.type(screen.getByLabelText("Caption"), "hello");
    expect(screen.getByText("3 / 100")).toBeInTheDocument();
    expect(screen.getByText("5 / 2,200")).toBeInTheDocument();
  });

  it("offers Instagram and YouTube checkboxes", () => {
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    expect(screen.getByRole("checkbox", { name: "Instagram" })).not.toBeChecked();
    expect(screen.getByRole("checkbox", { name: "YouTube" })).not.toBeChecked();
  });

  it("blocks an invalid form and lists the problems without calling the API", async () => {
    const fn = stubApi();
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Choose an MP4 video.");
    expect(alert).toHaveTextContent("Title is required.");
    expect(alert).toHaveTextContent("Choose at least one platform.");
    expect(alert).toHaveTextContent("Choose when to publish.");
    expect(apiCalls(fn)).toHaveLength(0);
  });

  it("uploads, PUTs, queues, then reports success", async () => {
    const fn = happyFetch();
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(apiCalls(fn).map((c) => (c as [string])[0])).toEqual(["/api/uploads", PUT_URL, "/api/posts"]);
    const body = JSON.parse((apiCalls(fn)[2] as [string, RequestInit])[1].body as string);
    expect(body).toMatchObject({
      video_key: "videos/x.mp4",
      title: "Sunday reel",
      caption: "Hello",
      platforms: ["instagram"],
      slot_at: new Date(2099, 2, 5, 7, 4).getTime() / 1000,
    });
  });

  it("shows the server's message when queueing fails, and keeps the form", async () => {
    stubApi({ api: [ticket(), new Response(""), new Response("that slot is already taken", { status: 409 })] });
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("that slot is already taken");
    expect(onQueued).not.toHaveBeenCalled();
    expect(screen.getByLabelText("Title")).toHaveValue("Sunday reel");
  });

});

describe("Upload retries and safety", () => {
  it("retries only the queue call after a queue failure, without uploading again", async () => {
    const fn = stubApi({
      api: [ticket(), new Response(""), new Response("that slot is already taken", { status: 409 }), new Response("{}", { status: 201 })],
    });
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await screen.findByRole("alert");
    fireEvent.change(screen.getByLabelText("When"), { target: { value: "2099-03-06T07:04" } });
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(apiCalls(fn).map((c) => (c as [string])[0])).toEqual([
      "/api/uploads",
      PUT_URL,
      "/api/posts",
      "/api/posts",
    ]);
    const retry = JSON.parse((apiCalls(fn)[3] as [string, RequestInit])[1].body as string);
    expect(retry.video_key).toBe("videos/x.mp4");
  });

  it("uploads again when a different file is chosen after a failure", async () => {
    const fn = stubApi({
      api: [
        ticket(),
        new Response(""),
        new Response("that slot is already taken", { status: 409 }),
        json({ video_key: "videos/y.mp4", put_url: PUT_URL }),
        new Response(""),
        new Response("{}", { status: 201 }),
      ],
    });
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await screen.findByRole("alert");
    await user.upload(
      screen.getByLabelText("Video (MP4)"),
      new File(["def"], "other.mp4", { type: "video/mp4" }),
    );
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(apiCalls(fn).map((c) => (c as [string])[0])).toEqual([
      "/api/uploads",
      PUT_URL,
      "/api/posts",
      "/api/uploads",
      PUT_URL,
      "/api/posts",
    ]);
    const body = JSON.parse((apiCalls(fn)[5] as [string, RequestInit])[1].body as string);
    expect(body.video_key).toBe("videos/y.mp4");
  });

  it("never puts the upload link in the page", async () => {
    stubApi({ api: [ticket(), new Response("denied", { status: 403 })] });
    const user = userEvent.setup();
    const { container } = render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
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
    const base = stubApi();
    vi.stubGlobal("fetch", (url: string) => (url.startsWith("/api/slots/") ? base(url) : pending));
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    expect(screen.getByRole("button", { name: "Queueing…" })).toBeDisabled();
    release(new Response("nope", { status: 500 }));
    await screen.findByRole("alert");
  });
});

async function fillWithoutWhen(user: ReturnType<typeof userEvent.setup>) {
  await user.upload(
    screen.getByLabelText("Video (MP4)"),
    new File(["abc"], "reel.mp4", { type: "video/mp4" }),
  );
  await user.type(screen.getByLabelText("Title"), "Sunday reel");
  await user.click(screen.getByLabelText("Instagram"));
}

const TAKEN = "that slot is already taken";
const slotsReply = (...slots: number[]) => json({ slots });
const queuedBody = (fn: ReturnType<typeof stubApi>) =>
  JSON.parse((apiCalls(fn).at(-1) as [string, RequestInit])[1].body as string);

describe("Upload slot picker: listing", () => {
  it("fetches the pattern and the next five slots together on mount", async () => {
    const fn = stubApi({ slotReplies: [slotsReply(SLOT_A)] });
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    await screen.findByRole("radio", { name: "Wed 04/03/2099 19:00" });
    const urls = fn.mock.calls.map((c) => c[0]);
    expect(urls).toHaveLength(2);
    expect(urls).toContain("/api/slots/pattern");
    expect(urls).toContain("/api/slots/next?count=5");
  });

  it("lists the slots in the pattern's offset with the first preselected", async () => {
    stubApi({ slotReplies: [slotsReply(SLOT_A, SLOT_B, SLOT_C)] });
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    const group = await screen.findByRole("group", { name: "When (Brisbane, Sydney/Melbourne standard UTC+10)" });
    const radios = within(group).getAllByRole("radio");
    expect(radios.map((r) => r.closest("label")?.textContent)).toEqual([
      "Wed 04/03/2099 19:00",
      "Fri 06/03/2099 08:30",
      "Wed 11/03/2099 19:00",
      "Pick another time",
    ]);
    expect(radios[0]).toBeChecked();
    expect(screen.queryByLabelText("Date and time")).not.toBeInTheDocument();
  });

});

describe("Upload slot picker: choosing", () => {
  it("queues with the first slot's Unix seconds by default", async () => {
    const fn = stubApi({
      slotReplies: [slotsReply(SLOT_A, SLOT_B)],
      api: [ticket(), new Response(""), new Response("{}", { status: 201 })],
    });
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    await screen.findByRole("radio", { name: "Wed 04/03/2099 19:00" });
    await fillWithoutWhen(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(queuedBody(fn).slot_at).toBe(SLOT_A);
  });

  it("queues with another listed slot when it is chosen", async () => {
    const fn = stubApi({
      slotReplies: [slotsReply(SLOT_A, SLOT_B)],
      api: [ticket(), new Response(""), new Response("{}", { status: 201 })],
    });
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    await user.click(await screen.findByRole("radio", { name: "Fri 06/03/2099 08:30" }));
    await fillWithoutWhen(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(queuedBody(fn).slot_at).toBe(SLOT_B);
  });

  it("reveals the date and time input for Pick another time and uses it", async () => {
    const fn = stubApi({
      slotReplies: [slotsReply(SLOT_A)],
      api: [ticket(), new Response(""), new Response("{}", { status: 201 })],
    });
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    await user.click(await screen.findByRole("radio", { name: "Pick another time" }));
    fireEvent.change(screen.getByLabelText("Date and time"), { target: { value: "2099-03-05T07:04" } });
    await fillWithoutWhen(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(queuedBody(fn).slot_at).toBe(new Date(2099, 2, 5, 7, 4).getTime() / 1000);
  });

  it("asks for a time when Pick another time is chosen but left empty", async () => {
    stubApi({ slotReplies: [slotsReply(SLOT_A)] });
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    await user.click(await screen.findByRole("radio", { name: "Pick another time" }));
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Choose when to publish.");
  });

});

describe("Upload slot picker: no slots", () => {
  it("falls back to the date and time input with a link to the Slots screen when there are no slots", async () => {
    stubApi({ slotReplies: [slotsReply()] });
    const onOpenSlots = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} onOpenSlots={onOpenSlots} />);
    expect(await screen.findByLabelText("When")).toHaveAttribute("type", "datetime-local");
    expect(screen.queryByRole("radio")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Set your weekly slots" }));
    expect(onOpenSlots).toHaveBeenCalledTimes(1);
  });

  it("falls back the same way when the slots cannot be fetched", async () => {
    stubApi({ slotReplies: [new Response("storage error", { status: 500 })] });
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    expect(await screen.findByLabelText("When")).toHaveAttribute("type", "datetime-local");
    expect(screen.getByRole("button", { name: "Set your weekly slots" })).toBeInTheDocument();
    expect(screen.queryByRole("radio")).not.toBeInTheDocument();
  });

});

describe("Upload slot picker: after a queue error", () => {
  it("refreshes the slots after a taken-slot error, preselects the first and does not upload again", async () => {
    const fn = stubApi({
      slotReplies: [slotsReply(SLOT_A, SLOT_B), slotsReply(SLOT_B, SLOT_C)],
      api: [
        ticket(),
        new Response(""),
        new Response(TAKEN, { status: 409 }),
        new Response("{}", { status: 201 }),
      ],
    });
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    await screen.findByRole("radio", { name: "Wed 04/03/2099 19:00" });
    await fillWithoutWhen(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(TAKEN);
    expect(screen.queryByRole("radio", { name: "Wed 04/03/2099 19:00" })).not.toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Fri 06/03/2099 08:30" })).toBeChecked();
    expect(fn.mock.calls.filter((c) => String(c[0]).startsWith("/api/slots/next"))).toHaveLength(2);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(apiCalls(fn).map((c) => c[0])).toEqual([
      "/api/uploads",
      PUT_URL,
      "/api/posts",
      "/api/posts",
    ]);
    expect(queuedBody(fn)).toMatchObject({ video_key: "videos/x.mp4", slot_at: SLOT_B });
  });

  it("does not refresh the slots after other queue errors", async () => {
    const fn = stubApi({
      slotReplies: [slotsReply(SLOT_A)],
      api: [ticket(), new Response(""), new Response("storage error", { status: 500 })],
    });
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    await screen.findByRole("radio", { name: "Wed 04/03/2099 19:00" });
    await fillWithoutWhen(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("storage error");
    expect(fn.mock.calls.filter((c) => String(c[0]).startsWith("/api/slots/next"))).toHaveLength(1);
  });
});

describe("Upload slot picker: zone in the legend", () => {
  it("names an offset that is not in the table as UTC+HH:MM", async () => {
    stubApi({ slotReplies: [slotsReply(SLOT_A)], offset: 345 });
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    expect(await screen.findByRole("group", { name: "When (UTC+5:45)" })).toBeInTheDocument();
  });
});

describe("Upload slot picker: a 409 on queueing", () => {
  const PICKED = "We have picked the next free slot.";

  async function submitWith409(message: string, chooseOther: boolean) {
    const fn = stubApi({
      slotReplies: [slotsReply(SLOT_A, SLOT_B), slotsReply(SLOT_B, SLOT_C)],
      api: [ticket(), new Response(""), new Response(message, { status: 409 })],
    });
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    await screen.findByRole("radio", { name: "Wed 04/03/2099 19:00" });
    if (chooseOther) {
      await user.click(screen.getByRole("radio", { name: "Pick another time" }));
      fireEvent.change(screen.getByLabelText("Date and time"), { target: { value: "2099-03-05T07:04" } });
    }
    await fillWithoutWhen(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await screen.findByRole("alert");
    return fn;
  }

  const nextCalls = (fn: ReturnType<typeof stubApi>) =>
    fn.mock.calls.filter((c) => String(c[0]).startsWith("/api/slots/next")).length;

  it("keeps Pick another time and the typed time, refreshes the list and adds no extra line", async () => {
    const fn = await submitWith409(TAKEN, true);
    await waitFor(() => expect(nextCalls(fn)).toBe(2));
    await screen.findByRole("radio", { name: "Wed 11/03/2099 19:00" });
    expect(screen.getByRole("radio", { name: "Pick another time" })).toBeChecked();
    expect(screen.getByLabelText("Date and time")).toHaveValue("2099-03-05T07:04");
    expect(screen.queryByRole("radio", { name: "Wed 04/03/2099 19:00" })).not.toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(TAKEN);
    expect(screen.getByRole("alert")).not.toHaveTextContent(PICKED);
  });

  it("moves a selected slot to the new first slot and says so", async () => {
    await submitWith409(TAKEN, false);
    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent(PICKED));
    expect(screen.getByRole("alert")).toHaveTextContent(TAKEN);
    expect(screen.getByRole("radio", { name: "Fri 06/03/2099 08:30" })).toBeChecked();
  });

  it("does not refresh the slots when the video is already queued", async () => {
    const fn = await submitWith409("that video is already queued", false);
    expect(screen.getByRole("alert")).toHaveTextContent("that video is already queued");
    expect(screen.getByRole("alert")).not.toHaveTextContent(PICKED);
    expect(nextCalls(fn)).toBe(1);
    expect(screen.getByRole("radio", { name: "Wed 04/03/2099 19:00" })).toBeChecked();
  });
});

describe("Upload slot picker: while the slots load", () => {
  /** The slots lookup waits until `release` is called (or forever). */
  function stubHeld(api: Response[] = []) {
    let release: (r: Response) => void = () => {};
    const gate = new Promise<Response>((r) => (release = r));
    const queue = [...api];
    const fn = vi.fn<(url: string) => Promise<Response>>((url) => {
      if (url === "/api/slots/pattern") return Promise.resolve(json({ utc_offset_minutes: 600, rules: [] }));
      if (url.startsWith("/api/slots/next")) return gate;
      return Promise.resolve(queue.shift() ?? new Response("unexpected", { status: 500 }));
    });
    vi.stubGlobal("fetch", fn);
    return { fn, release };
  }

  it("shows the date and time input, and queues with it when the lookup never resolves", async () => {
    const { fn } = stubHeld([ticket(), new Response(""), new Response("{}", { status: 201 })]);
    const onQueued = vi.fn();
    const user = userEvent.setup();
    render(<Upload onQueued={onQueued} onOpenSlots={vi.fn()} />);
    expect(screen.getByLabelText("When")).toHaveAttribute("type", "datetime-local");
    expect(screen.queryByText("Set your weekly slots")).not.toBeInTheDocument();
    await fillValid(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await waitFor(() => expect(onQueued).toHaveBeenCalledTimes(1));
    expect(fn.mock.calls.at(-1)?.[0]).toBe("/api/posts");
  });

  it("switches to the radio list when the slots arrive and the input is untouched", async () => {
    const { release } = stubHeld();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    expect(screen.getByLabelText("When")).toBeInTheDocument();
    release(slotsReply(SLOT_A));
    expect(await screen.findByRole("radio", { name: "Wed 04/03/2099 19:00" })).toBeChecked();
    expect(screen.queryByLabelText("Date and time")).not.toBeInTheDocument();
  });

  it("keeps the typed time under Pick another time when the slots arrive after typing", async () => {
    const { release } = stubHeld();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    const input = screen.getByLabelText("When");
    fireEvent.change(input, { target: { value: "2099-03-05T07:04" } });
    release(slotsReply(SLOT_A));
    expect(await screen.findByRole("radio", { name: "Pick another time" })).toBeChecked();
    expect(screen.getByLabelText("Date and time")).toBe(input);
    expect(input).toHaveValue("2099-03-05T07:04");
  });

  it("keeps the focused, empty input and preselects nothing when the slots arrive", async () => {
    const { release } = stubHeld();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    const input = screen.getByLabelText("When");
    input.focus();
    release(slotsReply(SLOT_A));
    expect(await screen.findByRole("radio", { name: "Pick another time" })).toBeChecked();
    expect(screen.getByRole("radio", { name: "Wed 04/03/2099 19:00" })).not.toBeChecked();
    expect(screen.getByLabelText("Date and time")).toBe(input);
    expect(input).toHaveFocus();
  });
});

describe("Upload slot picker: while busy", () => {
  it("disables the slot radios while the slots are refreshed after a 409", async () => {
    let release: (r: Response) => void = () => {};
    const gate = new Promise<Response>((r) => (release = r));
    let lookups = 0;
    const queue = [ticket(), new Response(""), new Response(TAKEN, { status: 409 })];
    vi.stubGlobal(
      "fetch",
      vi.fn<(url: string) => Promise<Response>>((url) => {
        if (url === "/api/slots/pattern") return Promise.resolve(json({ utc_offset_minutes: 600, rules: [] }));
        if (url.startsWith("/api/slots/next")) return ++lookups === 1 ? Promise.resolve(slotsReply(SLOT_A, SLOT_B)) : gate;
        return Promise.resolve(queue.shift() ?? new Response("unexpected", { status: 500 }));
      }),
    );
    const user = userEvent.setup();
    render(<Upload onQueued={vi.fn()} onOpenSlots={vi.fn()} />);
    await screen.findByRole("radio", { name: "Wed 04/03/2099 19:00" });
    await fillWithoutWhen(user);
    await user.click(screen.getByRole("button", { name: "Queue post" }));
    await screen.findByRole("alert");
    expect(screen.getByRole("radio", { name: "Fri 06/03/2099 08:30" })).toBeDisabled();
    release(slotsReply(SLOT_B, SLOT_C));
    await waitFor(() => expect(screen.getByRole("radio", { name: "Fri 06/03/2099 08:30" })).toBeEnabled());
  });
});
