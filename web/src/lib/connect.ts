// The YouTube connect flow as the browser sees it. The Worker redirects back to the
// Settings page with ?connected=1 or ?connect_error=<fixed code>. Only the fixed codes
// below are turned into text; anything else gets the generic line and is never echoed.

export const YOUTUBE_CONNECT_PATH = "/api/connect/youtube/start";

export const CONNECT_ERROR_MESSAGES: Record<string, string> = {
  access_denied: "You chose not to allow access, so YouTube was not connected.",
  bad_state: "That connection attempt expired or was already used. Please start again.",
  exchange_failed: "Google would not complete the connection. Please try again.",
  server_error: "Something went wrong on our side. Please try again.",
};

export const GENERIC_CONNECT_ERROR = "YouTube could not be connected. Please try again.";

export interface ConnectResult {
  connected: boolean;
  errorMessage: string | null;
}

export function readConnectResult(search: string): ConnectResult {
  const params = new URLSearchParams(search);
  const code = params.get("connect_error");
  if (code !== null) {
    const known = Object.hasOwn(CONNECT_ERROR_MESSAGES, code);
    return { connected: false, errorMessage: known ? (CONNECT_ERROR_MESSAGES[code] ?? GENERIC_CONNECT_ERROR) : GENERIC_CONNECT_ERROR };
  }
  return { connected: params.get("connected") === "1", errorMessage: null };
}
