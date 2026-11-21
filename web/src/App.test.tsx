import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "@/App";
import { PAYHIP_URL, PROMO_CTA } from "@/lib/brand";

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

  it("links the header brand and the footer promo link to Payhip on both screens", async () => {
    vi.stubGlobal("fetch", vi.fn().mockImplementation(() => Promise.resolve(new Response("[]"))));
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
  });
});
