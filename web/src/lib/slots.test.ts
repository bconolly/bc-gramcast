import { describe, expect, it } from "vitest";
import { UTC_OFFSET_OPTIONS, formatSlotInOffset } from "@/lib/slots";

// Expected strings were worked out independently with Python datetime and fixed offsets.
describe("formatSlotInOffset", () => {
  it("formats in the given offset as short weekday dd/mm/yyyy HH:mm", () => {
    expect(formatSlotInOffset(1795683600, 600)).toBe("Thu 26/11/2026 19:00");
  });

  it("handles a negative offset", () => {
    expect(formatSlotInOffset(1795707000, -300)).toBe("Thu 26/11/2026 10:30");
    expect(formatSlotInOffset(1795707000, -720)).toBe("Thu 26/11/2026 03:30");
  });

  it("moves to the next day across midnight with a positive offset", () => {
    expect(formatSlotInOffset(1795707000, 600)).toBe("Fri 27/11/2026 01:30");
    expect(formatSlotInOffset(1795707000, 570)).toBe("Fri 27/11/2026 01:00");
  });

  it("changes year across midnight", () => {
    expect(formatSlotInOffset(1798758000, 780)).toBe("Fri 01/01/2027 12:00");
    expect(formatSlotInOffset(1798758000, -720)).toBe("Thu 31/12/2026 11:00");
  });

  it("goes back a day with a negative offset", () => {
    expect(formatSlotInOffset(1772323200, -300)).toBe("Sat 28/02/2026 19:00");
  });

  it("does not depend on the browser's timezone", () => {
    const saved = process.env.TZ;
    try {
      for (const tz of ["Pacific/Auckland", "America/Los_Angeles"]) {
        process.env.TZ = tz;
        expect(formatSlotInOffset(1795683600, 600)).toBe("Thu 26/11/2026 19:00");
      }
    } finally {
      if (saved === undefined) delete process.env.TZ;
      else process.env.TZ = saved;
    }
  });
});

describe("UTC_OFFSET_OPTIONS", () => {
  it("lists the six Australasian offsets in minutes", () => {
    expect(UTC_OFFSET_OPTIONS.map((o) => o.minutes)).toEqual([480, 570, 600, 660, 720, 780]);
    expect(UTC_OFFSET_OPTIONS.map((o) => o.label)).toEqual([
      "Perth UTC+8",
      "Adelaide/Darwin UTC+9:30",
      "Brisbane UTC+10",
      "Sydney/Melbourne daylight saving UTC+11",
      "Auckland UTC+12",
      "Auckland daylight saving UTC+13",
    ]);
  });
});
