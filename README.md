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

Screenshots (needs the `playwright` package, which is not a devDependency, and Chromium installed; the scripts `require('playwright')`, for example from a global or a separate tools install): `npm --prefix web run build && node web/scripts/screenshots.cjs <out-dir>/` (the output directory is required; the script serves the build with `web/public/_headers` applied and fails on console errors, so CSP problems show up).

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
The owner connects their YouTube account once; the Worker keeps the token encrypted in D1. Publishing is described under Publishing below.

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

How it works: `GET /api/connect/youtube/start` makes a single-use random state (valid 10 minutes), stores it and sends you to Google asking only for the `youtube.upload` scope, with `access_type=offline` and `prompt=consent` so Google returns a refresh token. `GET /api/connect/youtube/callback` checks and uses up the state, trades the code for tokens, encrypts them (AES-256-GCM, a fresh random nonce per write, the provider name as authenticated data) and redirects to `/settings?connected=1`, or `/settings?connect_error=<code>` where the code is one of `access_denied`, `bad_state`, `exchange_failed`, `server_error`, `try_again`. `GET /api/connect/status` returns only `{"youtube":{"connected":bool,"needs_reconnect":bool}}`, from row presence and the flag; it never decrypts. A saved token that can no longer be read (for example after the key changed) shows as Connected until a scheduled run finds it unreadable, marks it needs-reconnect (only if the row is still the one it read) and the status then says so. No route returns a token, and nothing logs a code, token, state or URL.

`start` also refuses a `Sec-Fetch-Site` that is present and not `same-origin` or `none`: it answers 302 to `/settings?connect_error=try_again` ("Please press Connect YouTube again.") and starts nothing. A scheduled run's token refresh and its needs-reconnect mark apply only if the stored row is still the one that run loaded, so reconnecting while a run is in flight is never overwritten. If Google's reply names the granted scopes, they must include `youtube.upload`. Both connect routes are GET navigations behind the same owner sign-in as the rest of the API. The Origin check covers only POST, PUT, PATCH and DELETE, and the callback comes from Google, so what protects it is the single-use state. There is no PKCE (support for this flow was not confirmed).

Not verified: none of this has run against Google or Cloudflare. `DELETE ... RETURNING` and `UPDATE ... RETURNING` in D1 are assumed to work like SQLite's (the statements ran against SQLite 3.53.1 only). The D1 statements were run against SQLite and the Web Crypto call shapes against Node, not in the Workers runtime or D1. Whether the Access cookie is sent on the redirect back from Google, and the exact fields in Google's token reply, are untested here.

## Publishing
A Cron Trigger (`*/5 * * * *`) runs the Worker's `scheduled` handler, the only place anything is published, using the Worker's own clock. Each run handles at most ONE due post, as a YouTube Short, through the resumable upload. Fakes are used in all tests; nothing here has run against YouTube or Cloudflare.

| Var / secret | Default | What it does |
| --- | --- | --- |
| `PUBLISH_ENABLED` (var) | `"false"` | Fallback only, used while no settings have been saved in the app. Publishing then runs only when this is exactly `true`. Anything else logs `publish disabled`. |
| `PUBLISH_PRIVACY` (var) | `"private"` | Fallback only, as above. `private`, `unlisted` or `public`; unset or anything else is private. |
| cron | `*/5 * * * *` | `[triggers]` in `wrangler.toml`. |
| `YOUTUBE_CLIENT_ID`, `YOUTUBE_CLIENT_SECRET`, `TOKEN_ENC_KEY` | none | Secrets, as under Connect YouTube. The refresh uses the same ones. |

The publishing toggle: Connections has a Publishing card with an on/off switch and a privacy choice (private, unlisted or public). Saving it writes one row to the `settings` table (migration `0005_settings.sql`), and every scheduled run reads that row first, so a change applies from the next run with no deploy. Choosing public asks you to confirm. Saving also quotes the time of the settings you loaded, so if another tab or device saved in between, the save is refused with a message and the card reloads. Until you save once, the card shows the deploy-time vars (`PUBLISH_ENABLED`, `PUBLISH_PRIVACY`) as the settings in force; after that the vars are ignored. If the settings cannot be read or decoded, the run stops and logs `publish run failed: settings`; it never falls back to the vars on a bad read.

Deploy steps: apply migrations to the remote database BEFORE deploying (`npx wrangler d1 migrations apply gramcast --remote`, which now includes `0004_publish_state.sql`: it adds the state columns, existing posts become Queued with 0 attempts, and `0005_settings.sql`: it adds the `settings` table), then deploy. If you deploy first, every scheduled run stops with `publish run failed: settings` until the migration is applied, and the Publishing card shows an error. `wrangler.toml` ships the vars as `"false"` and `"private"`, so a deploy no longer needs `--var PUBLISH_ENABLED:true`: use the switch. If you were publishing with `--var PUBLISH_ENABLED:true`, a plain deploy of this build turns publishing off until you save once in Connections; after that save the vars are ignored.

The settings SQL (insert-if-absent, update only when `updated_at` still matches) is run against real SQLite by `crates/worker/tests/settings_sql.rs`, which applies migrations 0001 to 0005 first and skips itself if the `sqlite3` command is missing. CI does not install `sqlite3`, and whether the runner has it is unconfirmed. D1's own changed-row count (`meta.changes`) is still unverified: check during the first test that a save is accepted, and that a save from a stale tab is refused with the conflict message.

What a run does: lists due Queued posts (slot reached, any retry delay over) and Publishing posts; flags posts stuck in Publishing for 15 minutes as Failed (`stuck_publishing`, never retried: check YouTube); refreshes the token before the claim if it expires within 10 minutes, which is more than the longest publish can take (a refresh that cannot be saved, or whose save did not apply because you reconnected meanwhile, stops the run); claims the earliest due post with one compare-and-set UPDATE on every state column; uploads it; records the result the same way.

Limits (from Cloudflare's free plan as I understand it; unconfirmed in this environment): 50 subrequests per invocation, 10 ms CPU per invocation (network waiting does not count; whether moving bytes between R2 and fetch does is UNCONFIRMED, so a large video may hit it), 15 minutes for a cron invocation. Assumed: R2 and D1 binding calls count as subrequests. Budget: 18 reserved for D1 and token calls, 2 to start, 2 per chunk (R2 range read plus PUT) of 8 MiB, 8 for status checks, so at most 11 chunks: videos over 88 MiB are refused at upload time. No request (start, chunk, status check or forced refresh) is started 6 minutes after the claim, and each request times out after 30 seconds. The longest a publish can run is therefore 6 minutes + the 2 second status check wait + 30 seconds (392 s), asserted in the domain to be well inside the 15 minute stuck limit. Both numbers are our own choices. The deadline is absolute (claim time + 6 minutes) and is passed to the publisher, so the forced-refresh retry after a token refusal cannot extend a post past it. Running out of budget, status checks or time is `unknown` only once the final chunk was sent; before that nothing can exist at YouTube, so the post is retried. A refusal (4xx) of the final chunk (every video of 8 MiB or less is one chunk) or of any chunk once the final one was sent is `unknown` too, because the video may exist; a refusal of an earlier chunk cannot have created anything, so it is classified as usual (auth, rate limit, rejected). Behaviour on an expired token mid-upload is undocumented. Response bodies from YouTube and the token endpoint are read only if their `Content-Length` is 64 KiB or less; a response with NO `Content-Length` header is still read in full with no limit. Each range read from R2 uses the etag taken from `head()` (`onlyIf` etagMatches): if the video was replaced after queueing, R2 returns an object with no body and the post is retried (`transient`) rather than uploading a mixed video. At most 3 stuck posts are flagged per run.

Edit and delete are refused (409 `too close to its slot to change`) once a post's slot is within 120 seconds, or after it, and for any post the publisher has claimed; the Queue hides the buttons and says the same. YouTube posts also need a title of 100 characters or fewer, a caption of 5,000 bytes or fewer (bytes, not characters) and no `<` or `>` in either.

Log lines (fixed words only; never a token, URL, key, provider text or error text): `publish disabled`; `publish run failed: settings` (the settings could not be read; nothing was published); `publish run posts=N`; `publish post=<id> outcome=<code> [reason=<code>] [external_id=<id>]`; `publish halted=<code>`; `publish run failed: setup` or `: list`.
- Outcomes: `published`; `retrying` (put back with a delay); `failed` (see reasons); `already_claimed` (another run won the claim); `released` (claim undone because the token could not be refreshed, no attempt used); `flagged_stuck`; `state_lost` (the upload finished or failed but the final write did not land; the post stays Publishing and is flagged stuck later, so check YouTube).
- Halts (the run stopped early): `needs_reconnect` (no token, or Google refuses it or it cannot be read: reconnect in Connections); `token_unavailable` (refresh trouble that may pass); `rate_limited` (quota; the post waits an hour, no attempt used); `storage` (D1 failed).
- Failure reasons: `rejected` (YouTube refused the video or its text, or the video is empty or over the cap); `transient` (3 tries failed, nothing was posted); `unknown` (the upload ended with an unknown result, the video may exist: never retried, check YouTube Studio); `stuck_publishing`; `auth` and `rate_limited` are retried, not final.

### First real publish test
1. Apply migrations, set the secrets, deploy, open Connections and connect YouTube.
2. Queue ONE throwaway video (a few seconds, vertical) for a slot a few minutes ahead.
3. In Connections, switch Publishing on with privacy Private, and save. The `settings` migration must already be applied remotely (see Deploy steps).
4. Run `wrangler tail`. Within five minutes of the slot you should see `publish run posts=1` and `outcome=published external_id=...`.
5. Check YouTube Studio: a private Short with the post's title and description. The Queue shows Published with a link.
6. Delete the test video in Studio, switch Publishing off in Connections until you are ready, and choose Unlisted or Public there when you want those posts.

Unverified, check during that test: that `AbortSignal.timeout` exists in Workers; that a 308 with its `Range` header reaches `fetch` as an ordinary response; that the chunk PUTs want (or tolerate) the bearer token and no content type; the YouTube error reasons in `domain::youtube`: the documented ones are `uploadLimitExceeded` (400, the per-channel daily upload cap, a wait), `quotaExceeded` (403, a wait), the `videos.insert` 400s such as `invalidTitle` and 403s `forbiddenLicenseSetting` and `forbiddenPrivacySetting` (rejected), and `youtubeSignupRequired`, `authorizationRequired`, `insufficientPermissions` and `forbidden` (account-wide, stop the run); `dailyLimitExceeded` and `rateLimitExceeded` are not confirmed for `videos.insert` (kept as waits), and `accessNotConfigured` and `authError` are not in the YouTube documentation that was read (treated as project-wide so the run stops instead of failing a post per run; the banner then says to reconnect, which may not be the real fix); that `Retry-After` is delta-seconds (dates are ignored); `UPDATE ... RETURNING` in D1; and the CPU question above. A 404 on an upload session with the final chunk unsent is retried by the next run from the start rather than restarted in the same run. The Queue lists Published and Failed posts for a week after their slot; a Failed post cannot be edited or deleted.

What to watch in the first real test: the free-plan CPU question. Each chunk is copied through WebAssembly (R2 body to a `Vec` to a `Uint8Array`) before the PUT, and the free plan's cron CPU limit is 10 ms (network waiting does not count). Run `wrangler tail` and look for CPU or `exceeded` messages on the run; the Paid plan allows 5 minutes of CPU (30 seconds by default) for crons under an hour. Passing the R2 body straight to `fetch` is not confirmed to work with the `worker` 0.9 crate, so the copying was not changed. Instagram is not published yet: Instagram-only posts stay Queued.

## API
All endpoints answer only to the owner (see Run locally). Request bodies are JSON.
- `POST /api/uploads`, `POST /api/posts`, `GET /api/queue`: upload a video (88 MiB at most) and queue it. The queue answer carries each post's `publish: {state, external_id?, reason?}`.
- `PATCH /api/posts/{id}`: edit a queued, unclaimed post. Body: `title`, `caption`, `platforms`, `slot_at` (all required, same rules as queueing; `id` and `video_key` cannot be sent). Answers with the saved post; 404 for an unknown id, 409 if another post holds the slot or the post is no longer queued (its slot has passed or publishing has claimed it).
- `DELETE /api/posts/{id}`: delete a queued post and its video. Answers 204; 404 for an unknown id; 409 if the post is no longer queued or has been claimed (its video then stays). The post is removed first; if the video cannot be removed the call still succeeds and the Worker logs a fixed line with the post id.
- `GET /api/slots/pattern`: the weekly posting pattern, for example `{"utc_offset_minutes":600,"rules":[{"weekday":"wed","time":"19:00"}]}`. Before one is saved it is Brisbane (+600) with no rules.
- `PUT /api/slots/pattern`: save the pattern (same shape; weekday `mon` to `sun`, time 24h `HH:MM`, offset -720 to 840, at most 14 rules). Answers with the normalised pattern.
- `GET /api/settings/publish`: `{"enabled":false,"privacy":"private","updated_at":null,"source":"defaults"}`. `source` is `settings` once saved and `defaults` while the vars are in force (`updated_at` is then null).
- `PUT /api/settings/publish`: body `{"enabled":true,"privacy":"unlisted","expected_updated_at":null}`, all three required and no other field. `expected_updated_at` is the `updated_at` last read, or `null` when there was none. Answers with the saved settings (same shape); 400 for a bad body, 409 `settings changed since you loaded them` if someone saved since. Same Origin check as the other writes.
- `GET /api/connect/status`, `GET /api/connect/youtube/start`, `GET /api/connect/youtube/callback`: see Connect YouTube above. Connections screenshots: `node web/scripts/screenshots-connections.cjs <out-dir>/` (same Playwright requirement). Publishing card screenshots: `node web/scripts/screenshots-publish-settings.cjs <out-dir>/`.
- `GET /api/slots/next?count=N`: `{"slots":[<unix seconds>, ...]}`, the next pattern slots not already taken by a queued post. `count` is 1 to 20 and defaults to 5.

## Known gaps for later slices
- The slot pattern's UTC offset is fixed, so daylight saving is not adjusted (Brisbane has none). Change the offset by hand when your clocks change.
- The Upload screen gets the offset and the slots from two separate calls, so saving the pattern between them can show a stale offset. Fix later by returning the offset with the slots.
- The Slots screen has no retry after a load error and does not warn about unsaved edits.
- The time input follows the browser's locale for 12 or 24 hour display.
- Sign-in is built but untested on Cloudflare (see Sign-in above). Add a Miniflare or `wrangler dev` smoke test of the sign-in path with a fake key endpoint.
- Static assets are served without the Worker, so Access must cover the whole hostname; the assets contain no data.
- `next_slots` reads all posts. Later, query only posts with a future `slot_at`.
- The publish run re-reads the object's size and etag with `head()` and refuses 0 bytes or more than 88 MiB, and each range read insists on that etag, but it checks neither the type nor the content: the 15 minute upload link can still overwrite the object after the post is queued (the publish run then stops with `transient`, nothing is posted).
- A post put back in the queue after an `auth` or `rate_limited` result keeps its original slot, so it may go out days late (rate limits wait an hour per run; an `auth` stop lasts until you reconnect), and once that slot has passed it cannot be edited or deleted. Edit and delete are also locked in the 2 minutes before a slot.
- The free-plan CPU cost of copying each chunk through WebAssembly (R2 body to `Vec` to `Uint8Array`) is unverified: see the first real publish test above.
- Delete rejected and orphaned uploads (add an R2 lifecycle rule as a backstop). A video left behind when a post is deleted but the R2 delete fails is one of these.
- Request bodies have no size cap in the Worker yet (a PATCH or POST body is parsed whole). Add a limit, for example by checking `Content-Length`.
- After a failed edit save, focus is not returned to the form (the Save button is disabled while saving). Opening another post's Edit drops the unsaved changes in the first form without asking.
- Editing cannot replace the video: delete the post and upload again.
- The delete and edit adapters (D1 `get`, `update`, `delete`, R2 `delete_object`) are compile-checked only, not run against Cloudflare.
- Put Cloudflare Access in front of every hostname the Worker answers on (workers.dev, Preview URLs and any custom domain), or set `workers_dev = false` and `preview_urls = false`.
- Install `worker-build` in CI from a pinned, cached copy instead of building it each run.
