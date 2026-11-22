// Screenshots of the Queue with every publish state, and the reconnect banner.
// Usage: npm run build && node scripts/screenshots-publish.cjs <outDir>
// Needs Playwright with Chromium already installed (require('playwright')).
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const dist = path.join(__dirname, "..", "dist");
const outDir = process.argv[2];
if (!outDir) {
  console.error("Usage: node scripts/screenshots-publish.cjs <outDir>");
  process.exit(2);
}
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".png": "image/png" };

// Apply the same headers Cloudflare will (public/_headers), so a CSP problem shows up here.
function readHeaders() {
  const lines = fs.readFileSync(path.join(dist, "_headers"), "utf8").split("\n");
  const headers = {};
  for (const line of lines.filter((l) => /^\s+\S/.test(l))) {
    const i = line.indexOf(":");
    headers[line.slice(0, i).trim()] = line.slice(i + 1).trim();
  }
  return headers;
}

// Like the real app: a path that is not a file (here /settings) gets index.html.
function serve() {
  const headers = readHeaders();
  const server = http.createServer((req, res) => {
    let name = req.url.split("?")[0];
    if (!path.extname(name)) name = "/index.html";
    const file = path.join(dist, path.normalize(name));
    if (!(file === dist || file.startsWith(dist + path.sep)) || !fs.existsSync(file)) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { ...headers, "Content-Type": TYPES[path.extname(file)] || "application/octet-stream" });
    fs.createReadStream(file).pipe(res);
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

// Slots are Unix seconds; the published/failed ones are in the past, as they would be.
const LATER = Math.floor(Date.UTC(2099, 2, 5, 7, 4) / 1000);
const POSTS = [
  { id: "f1", video_key: "videos/f1.mp4", title: "Wednesday tip: three-second reset", caption: "Hi", platforms: ["youtube"], slot_at: 1731900000, status: "ready", publish: { state: "published", external_id: "dQw4w9WgXcQ" } },
  { id: "f2", video_key: "videos/f2.mp4", title: "Friday reel: before and after", caption: "Hi", platforms: ["youtube"], slot_at: 1732000000, status: "ready", publish: { state: "failed", reason: "unknown" } },
  { id: "f3", video_key: "videos/f3.mp4", title: "Monday quick win", caption: "Hi", platforms: ["youtube"], slot_at: 1732100000, status: "ready", publish: { state: "publishing" } },
  { id: "f4", video_key: "videos/f4.mp4", title: "Wednesday tip: the next one", caption: "Hi", platforms: ["instagram", "youtube"], slot_at: LATER, status: "ready", publish: { state: "queued" } },
];
const STATES = [
  { name: "queue-states", status: { connected: true, needs_reconnect: false }, wait: "Watch on YouTube" },
  { name: "queue-reconnect-banner", status: { connected: true, needs_reconnect: true }, wait: "YouTube needs to be reconnected." },
];

function fail(message) {
  console.error(message);
  process.exitCode = 1;
}

async function shoot(browser, base, label, viewport) {
  for (const state of STATES) {
    const ctx = await browser.newContext({ viewport, deviceScaleFactor: 2, locale: "en-AU" });
    const page = await ctx.newPage();
    page.on("console", (m) => m.type() === "error" && fail(`console error: ${m.text()}`));
    page.on("pageerror", (e) => fail(`page error: ${e.message}`));
    await page.route("**/api/connect/status", (r) =>
      r.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ youtube: state.status }) }),
    );
    await page.route("**/api/queue", (r) =>
      r.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(POSTS) }),
    );
    await page.goto(base);
    await page.getByText(state.wait, { exact: false }).first().waitFor();
    await page.mouse.move(0, 0);
    await page.screenshot({ path: path.join(outDir, `publish-${state.name}-${label}.png`), fullPage: true });
    await ctx.close();
  }
}

(async () => {
  fs.mkdirSync(outDir, { recursive: true });
  const server = await serve();
  const base = `http://127.0.0.1:${server.address().port}/`;
  const browser = await chromium.launch();
  try {
    await shoot(browser, base, "phone", { width: 390, height: 844 });
    await shoot(browser, base, "desktop", { width: 1280, height: 800 });
  } finally {
    await browser.close();
    server.close();
  }
})();
