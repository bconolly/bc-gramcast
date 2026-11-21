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
The browser PUTs the video straight to R2, so the bucket needs a CORS rule that allows PUT with the `Content-Type` header from your app's origin.

Screenshots (needs Playwright and Chromium installed): `npm --prefix web run build && node web/scripts/screenshots.cjs <out-dir>/`.

`spike/cpu` measures how long one R2 presign takes in V8 WebAssembly, to check the Workers free plan's 10 ms CPU limit.
