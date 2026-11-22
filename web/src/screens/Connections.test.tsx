import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Connections } from "@/screens/Connections";
import { CONNECT_ERROR_MESSAGES, GENERIC_CONNECT_ERROR } from "@/lib/connect";

afterEach(() => vi.unstubAllGlobals());

function stubStatus(connected: boolean, needsReconnect: boolean) {
  const body = { youtube: { connected, needs_reconnect: needsReconnect } };
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify(body))));
}

const none = { connected: false, errorMessage: null };

describe("Connections", () => {
  it("offers Connect YouTube as a plain link to the start route when not connected", async () => {
    stubStatus(false, false);
    render(<Connections result={none} />);
    expect(await screen.findByText("Not connected")).toBeInTheDocument();
    const link = screen.getByRole("link", { name: "Connect YouTube" });
    expect(link).toHaveAttribute("href", "/api/connect/youtube/start");
    expect(screen.getByRole("heading", { name: "Connections" })).toBeInTheDocument();
  });

  it("shows a connected account and offers to reconnect", async () => {
    stubStatus(true, false);
    render(<Connections result={none} />);
    expect(await screen.findByText("Connected")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Reconnect YouTube" })).toHaveAttribute(
      "href",
      "/api/connect/youtube/start",
    );
    expect(screen.queryByText(/needs reconnecting/i)).not.toBeInTheDocument();
  });

  it("says plainly that a refused token needs the owner to reconnect", async () => {
    stubStatus(true, true);
    render(<Connections result={none} />);
    expect(await screen.findByText("Needs reconnecting")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Google stopped accepting the saved sign-in, so videos will not be posted to YouTube until you reconnect.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/encryption key/)).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Reconnect YouTube" })).toBeInTheDocument();
  });

  it("shows a banner for a fixed connect error and keeps the connect link", async () => {
    stubStatus(false, false);
    render(<Connections result={{ connected: false, errorMessage: CONNECT_ERROR_MESSAGES.access_denied ?? "" }} />);
    expect(await screen.findByRole("alert")).toHaveTextContent(CONNECT_ERROR_MESSAGES.access_denied ?? "?");
    expect(screen.getByRole("link", { name: "Connect YouTube" })).toBeInTheDocument();
  });

  it("shows a success banner after connecting", async () => {
    stubStatus(true, false);
    render(<Connections result={{ connected: true, errorMessage: null }} />);
    expect(await screen.findByRole("status")).toHaveTextContent("YouTube is connected.");
  });

  it("shows the generic line for an unknown code", async () => {
    stubStatus(false, false);
    render(<Connections result={{ connected: false, errorMessage: GENERIC_CONNECT_ERROR }} />);
    expect(await screen.findByRole("alert")).toHaveTextContent(GENERIC_CONNECT_ERROR);
  });

  it("says so when the status cannot be loaded, and still offers the link", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("youtube connect is not configured", { status: 503 })));
    render(<Connections result={none} />);
    expect(await screen.findByText("youtube connect is not configured")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Connect YouTube" })).toBeInTheDocument();
  });

  it("asks for nothing but the status", async () => {
    stubStatus(false, false);
    render(<Connections result={none} />);
    await screen.findByText("Not connected");
    const fetchFn = vi.mocked(fetch);
    expect(fetchFn.mock.calls.map((c) => c[0])).toEqual(["/api/connect/status"]);
  });
});
