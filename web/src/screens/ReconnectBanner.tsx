import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import { getConnectStatus } from "@/lib/api";

/**
 * Shown on every screen but Connections while the Worker reports that YouTube refused the
 * saved sign-in. It reads only the status flags. If they cannot be read it shows nothing:
 * the Connections screen reports its own load errors.
 */
export function ReconnectBanner({ onOpenConnections }: { onOpenConnections: () => void }) {
  const [needs, setNeeds] = useState(false);
  useEffect(() => {
    let live = true;
    getConnectStatus().then(
      (s) => live && setNeeds(s.youtube.connected && s.youtube.needs_reconnect),
      () => undefined,
    );
    return () => {
      live = false;
    };
  }, []);
  if (!needs) return null;
  return (
    <div role="alert" className="border-destructive text-destructive mb-4 space-y-2 rounded-md border p-3 text-sm">
      <p>
        <strong>YouTube needs to be reconnected.</strong> Videos will not be posted to YouTube until you do.
      </p>
      <Button type="button" variant="outline" className="min-h-11" onClick={onOpenConnections}>
        Go to Connections
      </Button>
    </div>
  );
}
