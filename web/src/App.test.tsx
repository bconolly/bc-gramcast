import { render, screen } from "@testing-library/react";
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
});
