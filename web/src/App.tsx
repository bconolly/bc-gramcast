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
        <h1 className="text-xl font-semibold">Gramcast</h1>
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
    </div>
  );
}
