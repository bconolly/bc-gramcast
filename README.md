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
`wrangler.toml` holds placeholders only: no secrets, and no `LOCAL_DEV_NO_AUTH`. The API answers 503 until sign-in is set up (see Sign-in below), unless local dev mode is on. Turn it on for local use only, in `.dev.vars` (git-ignored, never commit it):
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
Never run `vite --host`, and never bind wrangler to anything but 127.0.0.1, while `LOCAL_DEV_NO_AUTH` is set: the API has no sign-in in that mode. The flag is ignored whenever `TEAM_DOMAIN` is set, and only loopback Host headers are accepted.

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

## Sign-in (Cloudflare Access)
Only the owner can use the API. Cloudflare Access sits in front of the site and signs the owner in; the Worker then checks the token Access attaches to each request, so a request that skips Access is refused too.

What the Worker checks on every `/api/*` request:
- The token is in the `Cf-Access-Jwt-Assertion` header (the cookie is not used), at most 8192 bytes.
- The header says `RS256` (anything else, including `none` and `HS256`, is refused) and a plain `kid`.
- The signature verifies against the public key with that `kid`, fetched from `https://<team-name>.cloudflareaccess.com/cdn-cgi/access/certs` (only that URL, never one from the token). Keys are cached for an hour in the isolate; an unknown `kid` refetches at most once a minute (a key id not found while a refetch is throttled answers 503, since a retry may work), and keys that cannot be refreshed stop being used after 24 hours. At most 10 keys and 64 KB are read, only RSA keys of 2048 bits or more.
- Then the claims: `iss` is exactly `https://<team-name>.cloudflareaccess.com`, `aud` contains the application's AUD tag, `exp` is in the future and `nbf` not in the future (30 seconds of clock leeway), and `email` equals `OWNER_EMAIL` (ignoring case). The signature is checked before the claims.
- For POST, PUT, PATCH and DELETE: `Sec-Fetch-Site` is not `cross-site`, and an `Origin` header, when present, is `https://<Host>` (`http://<Host>` only for a loopback Host).
- Every response carries `X-Content-Type-Options: nosniff`, `Cache-Control: no-store`, `Referrer-Policy: no-referrer` and a `default-src 'none'` CSP. Denials use fixed messages (401 "sign-in required" for every token problem, 403 "not allowed" for a different user, 503 if sign-in is not set up or the keys cannot be fetched). Tokens, claims and emails are never logged.

Setup, to do in the Cloudflare dashboard (not done yet):
1. Turn Access on. For the workers.dev address: Workers & Pages, your Worker, Settings, Domains & Routes, Enable Cloudflare Access (this also covers Preview URLs). For a custom domain: Zero Trust, Access controls, Applications, add a self-hosted application for the whole hostname with a policy that allows only you.
2. Find the AUD tag: Zero Trust, Access controls, Applications, Edit, Additional settings, Application Audience (AUD) Tag.
3. Give the Worker the two vars, `TEAM_DOMAIN` (`<team-name>.cloudflareaccess.com`) and `POLICY_AUD` (the AUD tag). `wrangler.toml` keeps them empty because the repo is public, so pass them when you deploy, for example `wrangler deploy --var TEAM_DOMAIN:<team-name>.cloudflareaccess.com --var POLICY_AUD:<aud-tag>`, and never commit the real values.
4. Set the owner's email as a secret: `wrangler secret put OWNER_EMAIL`.

The Worker also answers on workers.dev and on Preview URLs unless they are turned off, so Access must cover those hostnames too (the one-click option in step 1 does). `wrangler.toml` deliberately does not set `workers_dev` or `preview_urls`: the Wrangler docs list both keys (`workers_dev = false` disables the workers.dev URL; `preview_urls` is left unchanged when omitted), but setting them would switch off the workers.dev address the one-click path relies on. If you use a custom domain only, set both to `false` and redeploy.

Sources: [Validate JWTs](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/validating-json/) and [One-click Access for Workers](https://developers.cloudflare.com/changelog/post/2025-10-03-one-click-access-for-workers/).

Not verified yet: none of this has run against Cloudflare. The Web Crypto calls (`importKey` with a JWK, `verify` with RSASSA-PKCS1-v1_5 and SHA-256) were checked in Node 22 only, not in the Workers runtime. The real token's claims (for example whether `email` is present for your login method, and what a service token looks like) and the live key endpoint's response have not been seen. How to supply the vars at deploy time (the `--var` flag above, and whether a value set in the dashboard survives a deploy that carries empty ones) is from memory of wrangler, not checked. The 503 "sign-in not configured yet" answer is the safe state until the setup above is done.

## Connect YouTube
The owner connects their YouTube account once; the Worker keeps the token encrypted in D1. Nothing publishes yet.

Secrets (set with `wrangler secret put <NAME>`, never commit them):

| Secret | What it is |
| --- | --- |
| `YOUTUBE_CLIENT_ID` | The OAuth client id from your Google Cloud project. |
| `YOUTUBE_CLIENT_SECRET` | That client's secret. |
| `TOKEN_ENC_KEY` | Encrypts the stored token. Standard base64 of exactly 32 random bytes. Generate it with `openssl rand -base64 32`, then `wrangler secret put TOKEN_ENC_KEY`. Keep a copy: if it is lost or changed, the stored token cannot be read and you reconnect. |

`PUBLIC_BASE_URL` (a var in `wrangler.toml`, default `https://gramcast.gramcast.workers.dev`) is the app's public address. The redirect URI is that plus `/api/connect/youtube/callback`. Register exactly that URI as an authorised redirect URI on the Google OAuth client.

Steps:
1. Apply the migration (`wrangler d1 migrations apply gramcast`), which adds `oauth_state` and `oauth_tokens`.
2. Set the three secrets and register the redirect URI with Google.
3. Open the app, go to Connections (`/settings`), press Connect YouTube, and allow access on Google's page. You come back to Connections showing Connected.

How it works: `GET /api/connect/youtube/start` makes a single-use random state (valid 10 minutes), stores it and sends you to Google asking only for the `youtube.upload` scope, with `access_type=offline` and `prompt=consent` so Google returns a refresh token. `GET /api/connect/youtube/callback` checks and uses up the state, trades the code for tokens, encrypts them (AES-256-GCM, a fresh random nonce per write, the provider name as authenticated data) and redirects to `/settings?connected=1`, or `/settings?connect_error=<code>` where the code is one of `access_denied`, `bad_state`, `exchange_failed`, `server_error`. `GET /api/connect/status` returns only `{"youtube":{"connected":bool,"needs_reconnect":bool}}`. No route returns a token, and nothing logs a code, token, state or URL.

Both connect routes are GET navigations behind the same owner sign-in as the rest of the API. The Origin check covers only POST, PUT, PATCH and DELETE, and the callback comes from Google, so what protects it is the single-use state. There is no PKCE (support for this flow was not confirmed).

Not verified: none of this has run against Google or Cloudflare. The D1 statements were run against SQLite and the Web Crypto call shapes against Node, not in the Workers runtime or D1. Whether the Access cookie is sent on the redirect back from Google, and the exact fields in Google's token reply, are untested here. The granted scope in the reply is not checked.

## API
All endpoints answer only to the owner (see Run locally). Request bodies are JSON.
- `POST /api/uploads`, `POST /api/posts`, `GET /api/queue`: upload a video and queue it.
- `PATCH /api/posts/{id}`: edit a queued post. Body: `title`, `caption`, `platforms`, `slot_at` (all required, same rules as queueing; `id` and `video_key` cannot be sent). Answers with the saved post; 404 for an unknown id, 409 if another post holds the slot or the post is no longer queued (its slot has passed).
- `DELETE /api/posts/{id}`: delete a queued post and its video. Answers 204; 404 for an unknown id; 409 if the post is no longer queued. The post is removed first; if the video cannot be removed the call still succeeds and the Worker logs a fixed line with the post id.
- `GET /api/slots/pattern`: the weekly posting pattern, for example `{"utc_offset_minutes":600,"rules":[{"weekday":"wed","time":"19:00"}]}`. Before one is saved it is Brisbane (+600) with no rules.
- `PUT /api/slots/pattern`: save the pattern (same shape; weekday `mon` to `sun`, time 24h `HH:MM`, offset -720 to 840, at most 14 rules). Answers with the normalised pattern.
- `GET /api/connect/status`, `GET /api/connect/youtube/start`, `GET /api/connect/youtube/callback`: see Connect YouTube above.
- `GET /api/slots/next?count=N`: `{"slots":[<unix seconds>, ...]}`, the next pattern slots not already taken by a queued post. `count` is 1 to 20 and defaults to 5.

## Known gaps for later slices
- The slot pattern's UTC offset is fixed, so daylight saving is not adjusted (Brisbane has none). Change the offset by hand when your clocks change.
- The Upload screen gets the offset and the slots from two separate calls, so saving the pattern between them can show a stale offset. Fix later by returning the offset with the slots.
- The Slots screen has no retry after a load error and does not warn about unsaved edits.
- The time input follows the browser's locale for 12 or 24 hour display.
- Sign-in is built but untested on Cloudflare (see Sign-in above). Add a Miniflare or `wrangler dev` smoke test of the sign-in path with a fake key endpoint.
- Static assets are served without the Worker, so Access must cover the whole hostname; the assets contain no data.
- `next_slots` reads all posts. Later, query only posts with a future `slot_at`.
- Re-check the object's size and type with `head()` in the publish use case: the 15 minute upload link can still overwrite the object after the post is queued.
- Delete rejected and orphaned uploads (add an R2 lifecycle rule as a backstop). A video left behind when a post is deleted but the R2 delete fails is one of these.
- Editing is unrestricted because nothing publishes yet. When publishing lands, block edits and deletes for posts that are publishing or published.
- Request bodies have no size cap in the Worker yet (a PATCH or POST body is parsed whole). Add a limit, for example by checking `Content-Length`.
- After a failed edit save, focus is not returned to the form (the Save button is disabled while saving). Opening another post's Edit drops the unsaved changes in the first form without asking.
- Editing cannot replace the video: delete the post and upload again.
- The delete and edit adapters (D1 `get`, `update`, `delete`, R2 `delete_object`) are compile-checked only, not run against Cloudflare.
- Put Cloudflare Access in front of every hostname the Worker answers on (workers.dev, Preview URLs and any custom domain), or set `workers_dev = false` and `preview_urls = false`.
- Install `worker-build` in CI from a pinned, cached copy instead of building it each run.
- The "still queued" check in SQL uses the time the request started, so it does not stop an edit or delete that lands just after the slot passes. Close this when publishing lands (for example with a status column in the WHERE clause).
