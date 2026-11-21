import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "@/App";

afterEach(() => vi.unstubAllGlobals());

describe("App", () => {
  it("starts on the queue and switches to upload and back", async () => {
    vi.stubGlobal("fetch", vi.fn().mockImplementation(() => Promise.resolve(new Response("[]"))));
    const user = userEvent.setup();
    render(<App />);
    expect(await screen.findByText("Nothing queued yet.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Upload" }));
    expect(screen.getByLabelText("Title")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Queue" }));
    expect(await screen.findByText("Nothing queued yet.")).toBeInTheDocument();
  });

  it("shows the BC Guides logo in the header and a footer on both screens", async () => {
    vi.stubGlobal("fetch", vi.fn().mockImplementation(() => Promise.resolve(new Response("[]"))));
    const user = userEvent.setup();
    render(<App />);
    expect(within(screen.getByRole("banner")).getByAltText("BC Guides")).toBeInTheDocument();
    expect(screen.getByRole("contentinfo")).toHaveTextContent("A BC Guides tool");
    await user.click(screen.getByRole("button", { name: "Upload" }));
    expect(screen.getByRole("contentinfo")).toBeInTheDocument();
  });
});
