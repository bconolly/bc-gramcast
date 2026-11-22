// API client. The server stays the authority: its fixed error messages are
// shown as they come. The upload link (put_url) is a secret: it is never logged,
// never kept in state, and never put in an error message.

export type Platform = "instagram" | "youtube";

export interface PostJson {
  id: string;
  video_key: string;
  title: string;
  caption: string;
  platforms: Platform[];
  slot_at: number;
  status: "ready" | "needs_caption";
}

export interface StartUploadBody {
  file_name: string;
  content_type: "video/mp4";
  size_bytes: number;
}

export interface QueuePostBody {
  video_key: string;
  title: string;
  caption: string;
  platforms: Platform[];
  slot_at: number;
}

/** Everything an edit may change. The id and video are fixed. */
export interface EditPostBody {
  title: string;
  caption: string;
  platforms: Platform[];
  slot_at: number;
}

export interface SubmitInput {
  file: File;
  title: string;
  caption: string;
  platforms: Platform[];
  slot_at: number;
}

export class ApiError extends Error {
  readonly status: number | undefined;
  constructor(message: string, status?: number) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

/** The Worker's fixed 409 text for a slot someone else has taken. */
export const SLOT_TAKEN_MESSAGE = "that slot is already taken";

/** True for the 409 that means the chosen slot is gone (not "that video is already queued"). */
export const isSlotTaken = (e: unknown): boolean =>
  e instanceof ApiError && e.status === 409 && e.message === SLOT_TAKEN_MESSAGE;

const UPLOAD_FAILED = "The video could not be uploaded. Please try again.";
const MAX_SERVER_MESSAGE = 200;

/** The Worker's errors are short fixed plain-text messages. Anything else gets a generic line. */
async function failure(res: Response): Promise<ApiError> {
  const text = (await res.text().catch(() => "")).trim();
  const fixed = text.length > 0 && text.length <= MAX_SERVER_MESSAGE && !text.startsWith("<");
  return new ApiError(
    fixed ? text : `Something went wrong (error ${res.status}). Please try again.`,
    res.status,
  );
}

async function call(path: string, init?: RequestInit): Promise<Response> {
  let res: Response;
  try {
    res = await fetch(path, init);
  } catch {
    throw new ApiError("Could not reach the server. Check your connection and try again.");
  }
  if (!res.ok) throw await failure(res);
  return res;
}

function sendJson(method: "POST" | "PUT" | "PATCH", path: string, body: unknown): Promise<Response> {
  return call(path, {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

const postJson = (path: string, body: unknown) => sendJson("POST", path, body);

export type WeekdayId = "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun";

export interface PatternRule {
  weekday: WeekdayId;
  /** 24-hour HH:MM. */
  time: string;
}

export interface PatternJson {
  utc_offset_minutes: number;
  rules: PatternRule[];
}

export async function getQueue(): Promise<PostJson[]> {
  return (await call("/api/queue")).json();
}

export async function startUpload(
  body: StartUploadBody,
): Promise<{ video_key: string; put_url: string }> {
  return (await postJson("/api/uploads", body)).json();
}

/** PUTs straight to storage. The failure text is fixed so the link can't leak through it. */
export async function putVideo(putUrl: string, file: File): Promise<void> {
  let res: Response;
  try {
    res = await fetch(putUrl, {
      method: "PUT",
      headers: { "Content-Type": "video/mp4" },
      body: file,
    });
  } catch {
    throw new ApiError(UPLOAD_FAILED);
  }
  if (!res.ok) throw new ApiError(UPLOAD_FAILED, res.status);
}

export async function queuePost(body: QueuePostBody): Promise<void> {
  await postJson("/api/posts", body);
}

/**
 * Upload, PUT, then queue. Stops at the first failure. When `resume.videoKey` is
 * set the video is already in storage, so only the queue call runs. `onUploaded`
 * gets the key (never the upload link) once the PUT has succeeded.
 */
export async function submitPost(
  input: SubmitInput,
  resume: { videoKey?: string; onUploaded?: (videoKey: string) => void } = {},
): Promise<void> {
  let videoKey = resume.videoKey;
  if (videoKey === undefined) {
    const ticket = await startUpload({
      file_name: input.file.name,
      content_type: "video/mp4",
      size_bytes: input.file.size,
    });
    await putVideo(ticket.put_url, input.file);
    videoKey = ticket.video_key;
    resume.onUploaded?.(videoKey);
  }
  await queuePost({
    video_key: videoKey,
    title: input.title,
    caption: input.caption,
    platforms: input.platforms,
    slot_at: input.slot_at,
  });
}

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;

/** Reads a JSON body and checks its shape; any mismatch gets the generic failure line. */
async function readChecked<T>(res: Response, ok: (body: unknown) => body is T): Promise<T> {
  const body: unknown = await res.json().catch(() => undefined);
  if (!ok(body)) throw new ApiError(`Something went wrong (error ${res.status}). Please try again.`, res.status);
  return body;
}

const isPattern = (b: unknown): b is PatternJson =>
  isRecord(b) &&
  Number.isInteger(b.utc_offset_minutes) &&
  Array.isArray(b.rules) &&
  b.rules.every((r) => isRecord(r) && typeof r.weekday === "string" && typeof r.time === "string");

const isSlots = (b: unknown): b is { slots: number[] } =>
  isRecord(b) && Array.isArray(b.slots) && b.slots.every((n) => Number.isInteger(n));

export async function getPattern(): Promise<PatternJson> {
  return readChecked(await call("/api/slots/pattern"), isPattern);
}

/** Saves the weekly pattern. Resolves to the pattern as the server normalised it. */
export async function savePattern(pattern: PatternJson): Promise<PatternJson> {
  return (await sendJson("PUT", "/api/slots/pattern", pattern)).json();
}

/** The next free pattern slots, as Unix seconds in ascending order. */
export async function getNextSlots(count: number): Promise<number[]> {
  return (await readChecked(await call(`/api/slots/next?count=${count}`), isSlots)).slots;
}

const isPost = (b: unknown): b is PostJson =>
  isRecord(b) &&
  typeof b.id === "string" &&
  typeof b.title === "string" &&
  Array.isArray(b.platforms) &&
  Number.isInteger(b.slot_at);

const postPath = (id: string) => `/api/posts/${encodeURIComponent(id)}`;

/** Saves an edit. Resolves to the post as the server stored it. */
export async function updatePost(id: string, body: EditPostBody): Promise<PostJson> {
  return readChecked(await sendJson("PATCH", postPath(id), body), isPost);
}

/** Deletes a queued post and its video. */
export async function deletePost(id: string): Promise<void> {
  await call(postPath(id), { method: "DELETE" });
}
