import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "@/App";
import { PAYHIP_URL, PROMO_CTA } from "@/lib/brand";
import { CONNECT_ERROR_MESSAGES } from "@/lib/connect";

afterEach(() => {
  vi.unstubAllGlobals();
  window.history.replaceState(null, "", "/");
});

/** Answers each endpoint with a harmless empty body. */
function stubEmptyApi() {
  const bodies: Record<string, string> = {
    "/api/slots/pattern": '{"utc_offset_minutes":600,"rules":[]}',
    "/api/slots/next?count=5": '{"slots":[]}',
    "/api/connect/status": '{"youtube":{"connected":false,"needs_reconnect":false}}',
    "/api/settings/publish": '{"enabled":false,"privacy":"private","updated_at":null,"source":"defaults"}',
  };
  vi.stubGlobal(
    "fetch",
    vi.fn().mockImplementation((url: string) => Promise.resolve(new Response(bodies[url] ?? "[]"))),
  );
}

describe("App", () => {
  it("starts on the queue and switches to upload and back", async () => {
    stubEmptyApi();
    const user = userEvent.setup();
    render(<App />);
    expect(await screen.findByText("Nothing queued yet.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Upload" }));
    expect(screen.getByLabelText("Title")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Queue" }));
    expect(await screen.findByText("Nothing queued yet.")).toBeInTheDocument();
  });

  it("has Queue, Upload, Slots and Connections in the nav and opens the slots screen", async () => {
    stubEmptyApi();
    const user = userEvent.setup();
    render(<App />);
    const nav = screen.getByRole("navigation", { name: "Screens" });
    expect(within(nav).getAllByRole("button").map((b) => b.textContent)).toEqual(["Queue", "Upload", "Slots", "Connections"]);
    await user.click(within(nav).getByRole("button", { name: "Slots" }));
    expect(within(nav).getByRole("button", { name: "Slots" })).toHaveAttribute("aria-current", "page");
    expect(await screen.findByLabelText("UTC offset")).toBeInTheDocument();
  });

  it("links the header brand and the footer promo link to Payhip on all four screens", async () => {
    stubEmptyApi();
    const user = userEvent.setup();
    render(<App />);
    expect(await screen.findByText("Nothing queued yet.")).toBeInTheDocument();
    const expectLink = (link: HTMLElement) => {
      expect(link).toHaveAttribute("href", PAYHIP_URL);
      expect(link).toHaveAttribute("target", "_blank");
      expect(link.getAttribute("rel")).toContain("noopener");
      expect(link.getAttribute("rel")).toContain("noreferrer");
    };
    const expectBranding = () => {
      const banner = screen.getByRole("banner");
      const headerLink = within(banner).getByRole("link", { name: /^by BC Guides on Payhip \(opens in new tab\)$/ });
      expectLink(headerLink);
      expect(within(headerLink).getByText("by BC Guides")).toBeInTheDocument();
      expect(within(headerLink).getByRole("presentation")).toHaveAttribute("src", "/brand/bc-monogram.png");
      expect(within(banner).getByRole("heading", { name: "Gramcast" }).closest("a")).toBeNull();
      const footer = screen.getByRole("contentinfo");
      expect(footer).toHaveTextContent("Built with the Beyond vibe coding guide by BC Guides.");
      expect(within(footer).getByText("Beyond vibe coding").tagName).toBe("STRONG");
      expect(within(footer).getByRole("presentation")).toHaveAttribute("src", "/brand/bc-monogram.png");
      const cta = within(footer).getByRole("link", { name: /See the guide/ });
      expectLink(cta);
      expect(PROMO_CTA).toBe("See the guide");
      expect(cta).toHaveAccessibleName("See the guide (opens in new tab)");
      expect(within(footer).queryByText(/Shop the guides|Made by BC Guides/)).not.toBeInTheDocument();
      expect(screen.queryByAltText("BC Guides")).not.toBeInTheDocument();
    };
    expectBranding();
    await user.click(screen.getByRole("button", { name: "Upload" }));
    expect(screen.getByLabelText("Title")).toBeInTheDocument();
    expectBranding();
    await user.click(screen.getByRole("button", { name: "Slots" }));
    expectBranding();
    await user.click(screen.getByRole("button", { name: "Connections" }));
    expect(await screen.findByText("Not connected")).toBeInTheDocument();
    expectBranding();
  });

  it("opens the Connections screen at /settings and reads the connect result", async () => {
    stubEmptyApi();
    window.history.replaceState(null, "", "/settings?connect_error=bad_state");
    render(<App />);
    const nav = screen.getByRole("navigation", { name: "Screens" });
    expect(within(nav).getByRole("button", { name: "Connections" })).toHaveAttribute("aria-current", "page");
    expect(await screen.findByRole("alert")).toHaveTextContent(CONNECT_ERROR_MESSAGES.bad_state ?? "?");
    // The result is shown once: the query is dropped so a reload does not repeat it.
    expect(window.location.pathname).toBe("/settings");
    expect(window.location.search).toBe("");
  });

  it("drops the connect banner when the owner leaves the Connections screen", async () => {
    stubEmptyApi();
    window.history.replaceState(null, "", "/settings?connected=1");
    const user = userEvent.setup();
    render(<App />);
    expect(await screen.findByRole("status")).toHaveTextContent("YouTube is connected.");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    await user.click(screen.getByRole("button", { name: "Connections" }));
    expect(await screen.findByText("Not connected")).toBeInTheDocument();
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows the reconnect banner on the queue and not on Connections, and leads there", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation((url: string) =>
        Promise.resolve(
          new Response(url === "/api/connect/status" ? '{"youtube":{"connected":true,"needs_reconnect":true}}' : "[]"),
        ),
      ),
    );
    const user = userEvent.setup();
    render(<App />);
    expect(await screen.findByRole("alert")).toHaveTextContent("YouTube needs to be reconnected");
    expect(screen.getByRole("banner")).toBeInTheDocument(); // the BC Guides header stays
    await user.click(screen.getByRole("button", { name: "Go to Connections" }));
    expect(await screen.findByText("Needs reconnecting")).toBeInTheDocument();
    expect(screen.queryByText("YouTube needs to be reconnected")).not.toBeInTheDocument();
    expect(window.location.pathname).toBe("/settings");
  });

  it("keeps the address in step with the screen", async () => {
    stubEmptyApi();
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByRole("button", { name: "Connections" }));
    expect(window.location.pathname).toBe("/settings");
    await user.click(screen.getByRole("button", { name: "Queue" }));
    expect(window.location.pathname).toBe("/");
  });
});
