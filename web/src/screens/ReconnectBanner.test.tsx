import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ReconnectBanner } from "@/screens/ReconnectBanner";

afterEach(() => vi.unstubAllGlobals());

function stub(res: Response | Error) {
  const fn = vi.fn(async () => {
    if (res instanceof Error) throw res;
    return res;
  });
  vi.stubGlobal("fetch", fn);
  return fn;
}
const status = (connected: boolean, needs: boolean) =>
  new Response(JSON.stringify({ youtube: { connected, needs_reconnect: needs } }));

describe("ReconnectBanner", () => {
  it("tells the owner to reconnect, and goes to Connections, when the token needs it", async () => {
    const fn = stub(status(true, true));
    const open = vi.fn();
    const user = userEvent.setup();
    render(<ReconnectBanner onOpenConnections={open} />);
    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("YouTube needs to be reconnected");
    expect(banner).toHaveTextContent("Videos will not be posted to YouTube until you do");
    await user.click(screen.getByRole("button", { name: "Go to Connections" }));
    expect(open).toHaveBeenCalledOnce();
    expect(fn).toHaveBeenCalledWith("/api/connect/status", undefined);
  });

  it("shows nothing when YouTube is fine, not connected, or the status cannot be read", async () => {
    for (const res of [status(true, false), status(false, false), new Response("boom", { status: 500 }), new Error("offline")]) {
      stub(res);
      const { container, unmount } = render(<ReconnectBanner onOpenConnections={() => undefined} />);
      await waitFor(() => expect(globalThis.fetch).toHaveBeenCalled());
      await new Promise((r) => setTimeout(r, 0));
      expect(screen.queryByRole("alert")).not.toBeInTheDocument();
      expect(container).toBeEmptyDOMElement();
      unmount();
    }
  });
});
