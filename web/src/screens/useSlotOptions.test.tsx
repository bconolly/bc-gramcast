import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useSlotOptions } from "@/screens/useSlotOptions";

afterEach(() => vi.unstubAllGlobals());

const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200 });

describe("useSlotOptions.refresh", () => {
  it("lets the newest lookup win when an older one finishes late", async () => {
    const gates: ((r: Response) => void)[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn((url: string) =>
        url === "/api/slots/pattern"
          ? Promise.resolve(json({ utc_offset_minutes: 600, rules: [] }))
          : new Promise<Response>((r) => gates.push(r)),
      ),
    );
    const { result } = renderHook(() => useSlotOptions(false));
    await waitFor(() => expect(gates).toHaveLength(1));
    let second!: Promise<unknown>;
    act(() => {
      second = result.current.refresh(false);
    });
    await waitFor(() => expect(gates).toHaveLength(2));
    await act(async () => {
      gates[1]!(json({ slots: [200] }));
      await second;
    });
    expect(result.current.options).toEqual({ status: "ready", slots: [200], offset: 600 });
    await act(async () => {
      gates[0]!(json({ slots: [100] }));
      await Promise.resolve();
    });
    expect(result.current.options).toEqual({ status: "ready", slots: [200], offset: 600 });
    expect(result.current.choice).toBe(200);
  });
});
