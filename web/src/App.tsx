import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Queue } from "@/screens/Queue";
import { Upload } from "@/screens/Upload";

type Screen = "queue" | "upload";

export function App() {
  const [screen, setScreen] = useState<Screen>("queue");
  return (
    <div className="mx-auto min-h-screen max-w-2xl px-4 py-6">
      <header className="mb-6 flex items-center justify-between gap-4">
        <div className="flex items-center gap-3">
          <img src="/brand/bc-monogram.png" alt="" width={40} height={40} className="size-10" />
          <div>
            <h1 className="text-xl leading-tight font-semibold">Gramcast</h1>
            <p className="text-muted-foreground text-xs">by BC Guides</p>
          </div>
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
        <img src="/brand/bc-guides-logo-h.png" alt="BC Guides" width={206} height={64} className="h-16 w-auto" />
      </footer>
    </div>
  );
}
