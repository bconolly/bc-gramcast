// How a post's publishing state is shown. The Worker sends only fixed tags and codes;
// anything else is turned into a fixed line here and never echoed. The YouTube address
// is built from a fixed template and an id that passes the same shape check as the
// Worker's (1 to 32 of letters, digits, underscore and hyphen), never from a URL.

export type PublishState = "queued" | "publishing" | "published" | "failed";

export interface PublishInfo {
  state: PublishState;
  external_id?: string;
  reason?: string;
}

const SHORTS_URL = "https://www.youtube.com/shorts/";
const VIDEO_ID = /^[A-Za-z0-9_-]{1,32}$/;

/** The public link for a published Short, or null if the id is not a well-formed video id. */
export function youtubeShortsUrl(id: string | null | undefined): string | null {
  return typeof id === "string" && VIDEO_ID.test(id) ? `${SHORTS_URL}${id}` : null;
}

/** One fixed message per failure code the Worker can send. */
export const FAILURE_MESSAGES: Record<string, string> = {
  auth: "YouTube stopped accepting the saved sign-in. Reconnect it in Connections, then queue this video again.",
  rate_limited: "YouTube's upload limit was reached. Try queueing this video again later.",
  rejected: "YouTube refused this video or its title or description. Check them, then queue the video again as a new post.",
  transient: "Publishing did not work after several tries, and nothing was posted. Queue the video again as a new post.",
  stuck_publishing: "This post got stuck while publishing. Check YouTube Studio to see whether it went up before trying again.",
  unknown: "YouTube did not confirm the upload. Check YouTube Studio to see whether it went up before trying again.",
};

export const GENERIC_FAILURE = "Publishing failed. Check YouTube Studio before trying again.";

export function failureMessage(code: string | undefined): string {
  return code !== undefined && Object.hasOwn(FAILURE_MESSAGES, code)
    ? (FAILURE_MESSAGES[code] ?? GENERIC_FAILURE)
    : GENERIC_FAILURE;
}
