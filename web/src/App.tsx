import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Queue } from "@/screens/Queue";
import { Upload } from "@/screens/Upload";
import { PAYHIP_URL } from "@/lib/brand";

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
            aria-label="BC Guides on Payhip"
            className="text-muted-foreground mt-1 flex items-center gap-2 text-xs hover:underline"
          >
            <img src="/brand/bc-monogram.png" alt="" width={24} height={24} className="size-6" />
            <span>by BC Guides</span>
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
      <footer className="border-border mt-10 flex flex-col items-start justify-between gap-1 border-t pt-4 sm:flex-row sm:items-center">
        <div className="flex items-center gap-2 text-[13px] text-[#52525b]">
          <img src="/brand/bc-monogram.png" alt="" width={24} height={24} className="size-6" />
          <span>Made by BC Guides</span>
        </div>
        <a
          href={PAYHIP_URL}
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex min-h-11 items-center text-[13px] font-medium text-[#2b7363] hover:underline"
        >
          Shop the guides →
        </a>
      </footer>
    </div>
  );
}
