import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { Connections } from "@/screens/Connections";
import { Queue } from "@/screens/Queue";
import { Slots } from "@/screens/Slots";
import { Upload } from "@/screens/Upload";
import { PAYHIP_URL, PROMO_CTA } from "@/lib/brand";
import { readConnectResult, type ConnectResult } from "@/lib/connect";

type Screen = "queue" | "upload" | "slots" | "connections";

// There is no router: /settings is the Connections screen, anything else starts on the queue.
const SETTINGS_PATH = "/settings";
const pathFor = (screen: Screen) => (screen === "connections" ? SETTINGS_PATH : "/");

const NAV: { id: Screen; label: string }[] = [
  { id: "queue", label: "Queue" },
  { id: "upload", label: "Upload" },
  { id: "slots", label: "Slots" },
  { id: "connections", label: "Connections" },
];

function keepAddress(path: string) {
  try {
    window.history.replaceState(null, "", path);
  } catch {
    // The address is a convenience; the screen works without it.
  }
}

export function App() {
  const [screen, setScreen] = useState<Screen>(() =>
    window.location.pathname === SETTINGS_PATH ? "connections" : "queue",
  );
  // Read once: the Worker's redirect puts the result in the query string, which is then dropped.
  const [connectResult] = useState<ConnectResult>(() => readConnectResult(window.location.search));
  useEffect(() => {
    if (window.location.search !== "") keepAddress(window.location.pathname);
  }, []);
  const go = (next: Screen) => {
    setScreen(next);
    keepAddress(pathFor(next));
  };
  return (
    <div className="mx-auto min-h-screen max-w-2xl px-4 py-6">
      <header className="mb-6 flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <div>
          <h1 className="text-xl leading-tight font-semibold">Gramcast</h1>
          <a
            href={PAYHIP_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="text-muted-foreground -my-2.5 flex min-h-11 items-center gap-2 py-2.5 text-xs hover:underline"
          >
            <img src="/brand/bc-monogram.png" alt="" width={24} height={24} className="size-6" />
            <span>
              by BC Guides{" "}
              <span className="sr-only">on Payhip (opens in new tab)</span>
            </span>
          </a>
        </div>
        <nav className="flex flex-wrap gap-1" aria-label="Screens">
          {NAV.map((item) => (
            <Button
              key={item.id}
              variant={screen === item.id ? "default" : "ghost"}
              className="min-h-11"
              aria-current={screen === item.id ? "page" : undefined}
              onClick={() => go(item.id)}
            >
              {item.label}
            </Button>
          ))}
        </nav>
      </header>
      <main>
        {screen === "queue" && <Queue onOpenSlots={() => go("slots")} />}
        {screen === "upload" && <Upload onQueued={() => go("queue")} onOpenSlots={() => go("slots")} />}
        {screen === "slots" && <Slots />}
        {screen === "connections" && <Connections result={connectResult} />}
      </main>
      <footer className="border-border mt-10 border-t pt-4">
        <div className="border-border bg-card flex items-center gap-3 rounded-xl border p-3">
          <img src="/brand/bc-monogram.png" alt="" width={36} height={36} className="size-9 shrink-0" />
          <p className="text-muted-foreground min-w-0 flex-1 text-[13px]">
            Built with the <strong className="text-foreground font-semibold">Beyond vibe coding</strong> guide by BC
            Guides.
          </p>
          <a
            href={PAYHIP_URL}
            target="_blank"
            rel="noopener noreferrer"
            className="bg-brand-navy inline-flex min-h-11 shrink-0 items-center rounded-md px-3 text-[13px] font-medium text-white hover:opacity-90"
          >
            {PROMO_CTA}
            {" "}
            <span className="sr-only">(opens in new tab)</span>
          </a>
        </div>
      </footer>
    </div>
  );
}
