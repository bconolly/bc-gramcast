import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PublishingCard } from "@/screens/PublishingCard";

afterEach(() => vi.unstubAllGlobals());

const defaults = { enabled: false, privacy: "private", updated_at: null, source: "defaults" };
const saved = (over: object = {}) => ({ enabled: true, privacy: "private", updated_at: 100, source: "settings", ...over });
const json = (b: unknown, status = 200) => new Response(JSON.stringify(b), { status });

/** Answers each call from the queue for its method; fails the test on a call nobody queued. */
function stubApi(queue: { GET?: Response[]; PUT?: Response[] }) {
  const fn = vi.fn(async (_path: string, init?: RequestInit) => {
    const next = queue[(init?.method ?? "GET") as "GET" | "PUT"]?.shift();
    if (!next) throw new Error("unexpected call");
    return next;
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}

const puts = (fn: ReturnType<typeof stubApi>) =>
  fn.mock.calls.filter(([, init]) => init?.method === "PUT").map(([, init]) => JSON.parse(String(init?.body)));

describe("PublishingCard", () => {
  it("shows publishing off and private, and says defaults are in force", async () => {
    stubApi({ GET: [json(defaults)] });
    render(<PublishingCard />);
    const sw = await screen.findByRole("switch", { name: "Publishing" });
    expect(sw).toHaveAttribute("aria-checked", "false");
    expect(screen.getByLabelText("Privacy")).toHaveValue("private");
    expect(screen.getByText(/nothing has been saved yet/i)).toBeInTheDocument();
  });

  it("saves with expected_updated_at null the first time, then uses the saved time", async () => {
    const user = userEvent.setup();
    const fn = stubApi({
      GET: [json(defaults)],
      PUT: [json(saved({ updated_at: 200 })), json(saved({ enabled: false, updated_at: 201 }))],
    });
    render(<PublishingCard />);
    await user.click(await screen.findByRole("switch", { name: "Publishing" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Publishing settings saved.");
    await user.click(screen.getByRole("switch", { name: "Publishing" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(puts(fn)).toHaveLength(2));
    expect(puts(fn)).toEqual([
      { enabled: true, privacy: "private", expected_updated_at: null },
      { enabled: false, privacy: "private", expected_updated_at: 200 },
    ]);
  });

  it("saves unlisted without asking", async () => {
    const user = userEvent.setup();
    const fn = stubApi({ GET: [json(saved())], PUT: [json(saved({ privacy: "unlisted", updated_at: 101 }))] });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "unlisted");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByRole("status");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(puts(fn)).toEqual([{ enabled: true, privacy: "unlisted", expected_updated_at: 100 }]);
  });

  it("asks before saving public, and Cancel saves nothing and returns focus to Save", async () => {
    const user = userEvent.setup();
    const fn = stubApi({ GET: [json(saved())] });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "public");
    const save = screen.getByRole("button", { name: "Save" });
    await user.click(save);
    const dialog = await screen.findByRole("alertdialog", { name: /public/i });
    expect(dialog).toHaveTextContent(/anyone/i);
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(save).toHaveFocus();
    expect(puts(fn)).toEqual([]);
  });

  it("Escape closes the question and returns focus to Save", async () => {
    const user = userEvent.setup();
    stubApi({ GET: [json(saved())] });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "public");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByRole("alertdialog");
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toHaveFocus();
  });

  it("saves public only after the owner confirms, then returns focus to Save", async () => {
    const user = userEvent.setup();
    const fn = stubApi({ GET: [json(saved())], PUT: [json(saved({ privacy: "public", updated_at: 101 }))] });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "public");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Save as public" }));
    await screen.findByRole("status");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toHaveFocus();
    expect(puts(fn)).toEqual([{ enabled: true, privacy: "public", expected_updated_at: 100 }]);
  });

  it("on a conflict says so plainly and reloads what is stored", async () => {
    const user = userEvent.setup();
    const fn = stubApi({
      GET: [json(saved()), json(saved({ enabled: false, privacy: "unlisted", updated_at: 150 }))],
      PUT: [new Response("settings changed since you loaded them", { status: 409 })],
    });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "unlisted");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/changed since you opened them/i);
    await waitFor(() => expect(screen.getByRole("switch", { name: "Publishing" })).toHaveAttribute("aria-checked", "false"));
    expect(screen.getByLabelText("Privacy")).toHaveValue("unlisted");
    expect(fn.mock.calls.filter(([, i]) => (i?.method ?? "GET") === "GET")).toHaveLength(2);
  });

  it("shows another save failure and keeps the owner's choices", async () => {
    const user = userEvent.setup();
    stubApi({ GET: [json(saved())], PUT: [new Response("internal error", { status: 500 })] });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "unlisted");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("internal error");
    expect(screen.getByLabelText("Privacy")).toHaveValue("unlisted");
  });

  it("offers Retry when the load fails", async () => {
    const user = userEvent.setup();
    stubApi({ GET: [new Response("internal error", { status: 500 }), json(saved())] });
    render(<PublishingCard />);
    expect(await screen.findByRole("alert")).toHaveTextContent("internal error");
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("switch", { name: "Publishing" })).toHaveAttribute("aria-checked", "true");
  });

  it("does not ask when switching off while public", async () => {
    const user = userEvent.setup();
    const pub = saved({ privacy: "public" });
    const fn = stubApi({ GET: [json(pub)], PUT: [json({ ...pub, enabled: false, updated_at: 101 })] });
    render(<PublishingCard />);
    await user.click(await screen.findByRole("switch", { name: "Publishing" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByRole("status");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(puts(fn)).toEqual([{ enabled: false, privacy: "public", expected_updated_at: 100 }]);
  });

  it("does not ask when public and on were already saved", async () => {
    const user = userEvent.setup();
    const pub = saved({ privacy: "public" });
    const fn = stubApi({ GET: [json(pub)], PUT: [json({ ...pub, updated_at: 101 })] });
    render(<PublishingCard />);
    await user.click(await screen.findByRole("button", { name: "Save" }));
    await screen.findByRole("status");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(puts(fn)).toHaveLength(1);
  });

  it("asks when switching on while public is already chosen, but not while off", async () => {
    const user = userEvent.setup();
    const offPublic = saved({ enabled: false, privacy: "public" });
    const fn = stubApi({ GET: [json(offPublic)], PUT: [json({ ...offPublic, updated_at: 101 })] });
    render(<PublishingCard />);
    await user.click(await screen.findByRole("button", { name: "Save" }));
    await screen.findByRole("status");
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(puts(fn)).toHaveLength(1);
    await user.click(screen.getByRole("switch", { name: "Publishing" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
    expect(puts(fn)).toHaveLength(1);
  });

  it("keeps the form up through a conflict reload and puts focus on Save", async () => {
    const user = userEvent.setup();
    let release: (r: Response) => void = () => {};
    const reloadAnswer = new Promise<Response>((resolve) => (release = resolve));
    const calls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_p: string, init?: RequestInit) => {
        const method = init?.method ?? "GET";
        calls.push(method);
        if (method === "PUT") return new Response("settings changed since you loaded them", { status: 409 });
        return calls.filter((m) => m === "GET").length === 1 ? json(saved()) : reloadAnswer;
      }),
    );
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "unlisted");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(calls).toEqual(["GET", "PUT", "GET"]));
    // The reload is still in flight: the form has not gone away.
    expect(screen.getByRole("switch", { name: "Publishing" })).toBeInTheDocument();
    release(json(saved({ privacy: "private", updated_at: 150 })));
    expect(await screen.findByRole("alert")).toHaveTextContent(/changed since you opened them/i);
    await waitFor(() => expect(screen.getByRole("button", { name: "Save" })).toHaveFocus());
  });

  it("says the save was refused and could not be reloaded when a conflict is followed by a failed reload", async () => {
    const user = userEvent.setup();
    stubApi({
      GET: [json(saved()), new Response("internal error", { status: 500 })],
      PUT: [new Response("settings changed since you loaded them", { status: 409 })],
    });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "unlisted");
    await user.click(screen.getByRole("button", { name: "Save" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("These settings were changed elsewhere and could not be reloaded");
    expect(alert).not.toHaveTextContent("internal error");
    expect(screen.getByRole("button", { name: "Retry" })).toBeInTheDocument();
  });

  it("keeps the conflict notice, once, after a Retry that follows a conflict", async () => {
    const user = userEvent.setup();
    stubApi({
      GET: [json(saved()), new Response("internal error", { status: 500 }), json(saved({ updated_at: 150 }))],
      PUT: [new Response("settings changed since you loaded them", { status: 409 })],
    });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "unlisted");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText(/could not be reloaded/i);
    await user.click(screen.getByRole("button", { name: "Retry" }));
    await screen.findByRole("switch", { name: "Publishing" });
    expect(screen.getAllByRole("alert")).toHaveLength(1);
    expect(screen.getByRole("alert")).toHaveTextContent(/changed since you opened them/i);
  });

  it("puts focus on Save when a Retry succeeds after a conflict whose reload failed", async () => {
    const user = userEvent.setup();
    stubApi({
      GET: [json(saved()), new Response("internal error", { status: 500 }), json(saved({ updated_at: 150 }))],
      PUT: [new Response("settings changed since you loaded them", { status: 409 })],
    });
    render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "unlisted");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await user.click(await screen.findByRole("button", { name: "Retry" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Save" })).toHaveFocus());
  });

  it("shows no conflict notice when a Retry follows an ordinary load failure", async () => {
    const user = userEvent.setup();
    stubApi({ GET: [new Response("internal error", { status: 500 }), json(saved())] });
    render(<PublishingCard />);
    await user.click(await screen.findByRole("button", { name: "Retry" }));
    await screen.findByRole("switch", { name: "Publishing" });
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("logs no error when the card is removed while a conflict reload is still pending", async () => {
    const user = userEvent.setup();
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    let release: (r: Response) => void = () => {};
    const pending = new Promise<Response>((resolve) => (release = resolve));
    let gets = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (_p: string, init?: RequestInit) => {
        if (init?.method === "PUT") return new Response("settings changed since you loaded them", { status: 409 });
        return ++gets === 1 ? json(saved()) : pending;
      }),
    );
    const { unmount } = render(<PublishingCard />);
    await user.selectOptions(await screen.findByLabelText("Privacy"), "unlisted");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(gets).toBe(2));
    unmount();
    release(json(saved({ updated_at: 150 })));
    await new Promise((r) => setTimeout(r, 0));
    expect(errors).not.toHaveBeenCalled();
    errors.mockRestore();
  });
});
