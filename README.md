# Gramcast

Self-hosted scheduler that auto-publishes the same video as an Instagram Reel and a YouTube Short on a weekly slot pattern, with platform analytics and Payhip sales in one place. Runs on Cloudflare's free tiers.

Work in progress. Slice 1 (upload and queue) is under way.

## Develop
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Front end (`web/`, Vite + React + TypeScript + Tailwind; Node 22.13 or newer):
```
cd web
npm ci
npm run lint && npm test && npm run build
```

## Run locally
`wrangler.toml` holds placeholders only: no secrets, and no `LOCAL_DEV_NO_AUTH`. Until owner sign-in exists the API answers 503 unless local dev mode is on. Turn it on for local use only, in `.dev.vars` (git-ignored, never commit it):
```
LOCAL_DEV_NO_AUTH=true
R2_ACCESS_KEY_ID=...
R2_SECRET_ACCESS_KEY=...
```
Run `wrangler dev` bound to loopback so nothing else on your network can reach the no-auth API:
```
npm --prefix web run build
wrangler dev --ip 127.0.0.1
```
Never run `vite --host`, and never bind wrangler to anything but 127.0.0.1, while `LOCAL_DEV_NO_AUTH` is set: the API has no sign-in in that mode.

`R2_BUCKET` (a var) and the `VIDEOS` binding's `bucket_name` in `wrangler.toml` must name the same bucket. The binding is for the Worker's own reads; the var is used to build presigned URLs.

The browser PUTs the video straight to R2, so the bucket needs a CORS rule. Use the specific origins, never `"*"` (replace the production placeholder with your real hostname):
```json
[
  {
    "AllowedOrigins": [
      "http://127.0.0.1:8787",
      "http://localhost:8787",
      "https://gramcast.example.com"
    ],
    "AllowedMethods": ["PUT"],
    "AllowedHeaders": ["Content-Type"],
    "MaxAgeSeconds": 3600
  }
]
```

Dates: the date-time picker follows the browser's locale for display (so it may show month first); the queue always shows dd/mm/yyyy.

Security headers for the static app live in `web/public/_headers` (CSP, nosniff, no-referrer). The CSP needs no `'unsafe-inline'`: the build has no inline scripts or styles.

Screenshots (needs Playwright and Chromium installed): `npm --prefix web run build && node web/scripts/screenshots.cjs <out-dir>/` (the output directory is required; the script serves the build with `web/public/_headers` applied and fails on console errors, so CSP problems show up).

`spike/cpu` measures how long one R2 presign takes in V8 WebAssembly, to check the Workers free plan's 10 ms CPU limit.

## API
All endpoints answer only to the owner (see Run locally). Request bodies are JSON.
- `POST /api/uploads`, `POST /api/posts`, `GET /api/queue`: upload a video and queue it.
- `GET /api/slots/pattern`: the weekly posting pattern, for example `{"utc_offset_minutes":600,"rules":[{"weekday":"wed","time":"19:00"}]}`. Before one is saved it is Brisbane (+600) with no rules.
- `PUT /api/slots/pattern`: save the pattern (same shape; weekday `mon` to `sun`, time 24h `HH:MM`, offset -720 to 840, at most 14 rules). Answers with the normalised pattern.
- `GET /api/slots/next?count=N`: `{"slots":[<unix seconds>, ...]}`, the next pattern slots not already taken by a queued post. `count` is 1 to 20 and defaults to 5.

## Known gaps for later slices
- The slot pattern's UTC offset is fixed, so daylight saving is not adjusted (Brisbane has none). Change the offset by hand when your clocks change.
- The Upload screen gets the offset and the slots from two separate calls, so saving the pattern between them can show a stale offset. Fix later by returning the offset with the slots.
- The Slots screen has no retry after a load error and does not warn about unsaved edits.
- The time input follows the browser's locale for 12 or 24 hour display.
- Carry-over from review: add `nosniff` and `no-store` headers on `/api` responses, and re-check the object size at publish (see below).
- `next_slots` reads all posts. Later, query only posts with a future `slot_at`.
- Re-check the object's size and type with `head()` in the publish use case: the 15 minute upload link can still overwrite the object after the post is queued.
- Delete rejected and orphaned uploads (add an R2 lifecycle rule as a backstop).
- In the sign-in slice, put Cloudflare Access in front of every hostname, or set `workers_dev = false` and `preview_urls = false`, and validate the Access JWT in `require_owner`.
- Install `worker-build` in CI from a pinned, cached copy instead of building it each run.
