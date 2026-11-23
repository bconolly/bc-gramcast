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
});
