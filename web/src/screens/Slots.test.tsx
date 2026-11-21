import { fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Slots } from "@/screens/Slots";

afterEach(() => vi.unstubAllGlobals());

const WED = { weekday: "wed", time: "19:00" };

function stub(pattern: object, ...more: Response[]) {
  const fn = vi.fn().mockResolvedValueOnce(new Response(JSON.stringify(pattern)));
  for (const r of more) fn.mockResolvedValueOnce(r);
  vi.stubGlobal("fetch", fn);
  return fn;
}

async function addRule(user: ReturnType<typeof userEvent.setup>, day: string, time: string) {
  await user.selectOptions(screen.getByLabelText("Weekday"), day);
  fireEvent.change(screen.getByLabelText("Time"), { target: { value: time } });
  await user.click(screen.getByRole("button", { name: "Add slot" }));
}

describe("Slots", () => {
  it("shows the saved rules and offset", async () => {
    stub({ utc_offset_minutes: 570, rules: [WED] });
    render(<Slots />);
    expect(await screen.findByText("Wednesday 19:00")).toBeInTheDocument();
    expect(screen.getByLabelText("UTC offset")).toHaveValue("570");
    expect(screen.getByText("The offset is fixed. Change it when the clocks change.")).toBeInTheDocument();
  });

  it("says so when there are no rules and defaults to Brisbane", async () => {
    stub({ utc_offset_minutes: 600, rules: [] });
    render(<Slots />);
    expect(await screen.findByText("No weekly slots yet.")).toBeInTheDocument();
    expect(screen.getByLabelText("UTC offset")).toHaveValue("600");
  });

  it("offers the six offsets", async () => {
    stub({ utc_offset_minutes: 600, rules: [] });
    render(<Slots />);
    const select = await screen.findByLabelText("UTC offset");
    expect(within(select).getAllByRole("option").map((o) => (o as HTMLOptionElement).value)).toEqual([
      "480", "570", "600", "660", "720", "780",
    ]);
  });

  it("offers Monday to Sunday in the weekday select", async () => {
    stub({ utc_offset_minutes: 600, rules: [] });
    render(<Slots />);
    const select = await screen.findByLabelText("Weekday");
    expect(within(select).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday",
    ]);
  });

  it("adds a rule, keeping the list in week order", async () => {
    stub({ utc_offset_minutes: 600, rules: [WED] });
    const user = userEvent.setup();
    render(<Slots />);
    await screen.findByText("Wednesday 19:00");
    await addRule(user, "mon", "06:30");
    const items = screen.getAllByRole("listitem").map((li) => li.textContent);
    expect(items[0]).toContain("Monday 06:30");
    expect(items[1]).toContain("Wednesday 19:00");
  });

  it("does not add a duplicate or an empty time", async () => {
    stub({ utc_offset_minutes: 600, rules: [WED] });
    const user = userEvent.setup();
    render(<Slots />);
    await screen.findByText("Wednesday 19:00");
    await addRule(user, "wed", "19:00");
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByRole("alert")).toHaveTextContent("That slot is already in your list.");
    await user.selectOptions(screen.getByLabelText("Weekday"), "fri");
    fireEvent.change(screen.getByLabelText("Time"), { target: { value: "" } });
    await user.click(screen.getByRole("button", { name: "Add slot" }));
    expect(screen.getAllByRole("listitem")).toHaveLength(1);
    expect(screen.getByRole("alert")).toHaveTextContent("Choose a time.");
  });

  it("removes a rule", async () => {
    stub({ utc_offset_minutes: 600, rules: [WED, { weekday: "fri", time: "08:00" }] });
    const user = userEvent.setup();
    render(<Slots />);
    await screen.findByText("Wednesday 19:00");
    await user.click(screen.getByRole("button", { name: "Remove Wednesday 19:00" }));
    expect(screen.queryByText("Wednesday 19:00")).not.toBeInTheDocument();
    expect(screen.getByText("Friday 08:00")).toBeInTheDocument();
  });

  it("stops adding at 14 rules and re-enables after a removal", async () => {
    const rules = Array.from({ length: 14 }, (_, i) => ({ weekday: "mon", time: `${String(i).padStart(2, "0")}:00` }));
    stub({ utc_offset_minutes: 600, rules });
    const user = userEvent.setup();
    render(<Slots />);
    await screen.findByText("Monday 00:00");
    expect(screen.getByRole("button", { name: "Add slot" })).toBeDisabled();
    expect(screen.getByText("14 of 14 slots. Remove one to add another.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Remove Monday 13:00" }));
    expect(screen.getByRole("button", { name: "Add slot" })).toBeEnabled();
  });

  it("saves with PUT and shows Saved.", async () => {
    const saved = { utc_offset_minutes: 780, rules: [{ weekday: "mon", time: "06:30" }, WED] };
    const fn = stub({ utc_offset_minutes: 600, rules: [WED] }, new Response(JSON.stringify(saved)));
    const user = userEvent.setup();
    render(<Slots />);
    await screen.findByText("Wednesday 19:00");
    await addRule(user, "mon", "06:30");
    await user.selectOptions(screen.getByLabelText("UTC offset"), "780");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    const [url, init] = fn.mock.calls[1] as [string, RequestInit];
    expect(url).toBe("/api/slots/pattern");
    expect(init.method).toBe("PUT");
    expect(new Headers(init.headers).get("Content-Type")).toBe("application/json");
    expect(JSON.parse(init.body as string)).toEqual({
      utc_offset_minutes: 780,
      rules: [{ weekday: "mon", time: "06:30" }, WED],
    });
  });

  it("shows the server's fixed message when saving fails, and keeps the edits", async () => {
    stub({ utc_offset_minutes: 600, rules: [WED] }, new Response("storage error", { status: 500 }));
    const user = userEvent.setup();
    render(<Slots />);
    await screen.findByText("Wednesday 19:00");
    await addRule(user, "mon", "06:30");
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("storage error")).toBeInTheDocument();
    expect(screen.queryByText("Saved.")).not.toBeInTheDocument();
    expect(screen.getByText("Monday 06:30")).toBeInTheDocument();
  });

  it("clears Saved. once the pattern is edited again", async () => {
    stub({ utc_offset_minutes: 600, rules: [WED] }, new Response(JSON.stringify({ utc_offset_minutes: 600, rules: [WED] })));
    const user = userEvent.setup();
    render(<Slots />);
    await screen.findByText("Wednesday 19:00");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await screen.findByText("Saved.");
    await user.click(screen.getByRole("button", { name: "Remove Wednesday 19:00" }));
    expect(screen.queryByText("Saved.")).not.toBeInTheDocument();
  });

  it("shows the message when the pattern cannot be loaded", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("storage error", { status: 500 })));
    render(<Slots />);
    expect(await screen.findByRole("alert")).toHaveTextContent("storage error");
  });

  it("keeps an offset outside the six options rather than losing it", async () => {
    stub({ utc_offset_minutes: -300, rules: [] });
    render(<Slots />);
    expect(await screen.findByLabelText("UTC offset")).toHaveValue("-300");
    expect(screen.getByRole("option", { name: "UTC-5" })).toBeInTheDocument();
  });
});
