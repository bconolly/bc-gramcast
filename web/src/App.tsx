import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Queue } from "@/screens/Queue";
import { Upload } from "@/screens/Upload";
import { PAYHIP_URL, PROMO_CTA } from "@/lib/brand";

type Screen = "queue" | "upload";

export function App() {
  const [screen, setScreen] = useState<Screen>("queue");
  return (
    <div className="mx-auto min-h-screen max-w-2xl px-4 py-6">
      <header className="mb-6 flex items-center justify-between gap-4">
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
        <nav className="flex gap-1" aria-label="Screens">
          <Button
            variant={screen === "queue" ? "default" : "ghost"}
            aria-current={screen === "queue" ? "page" : undefined}
            onClick={() => setScreen("queue")}
          >
            Queue
          </Button>
          <Button
            variant={screen === "upload" ? "default" : "ghost"}
            aria-current={screen === "upload" ? "page" : undefined}
            onClick={() => setScreen("upload")}
          >
            Upload
          </Button>
        </nav>
      </header>
      <main>
        {screen === "queue" ? <Queue /> : <Upload onQueued={() => setScreen("queue")} />}
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
