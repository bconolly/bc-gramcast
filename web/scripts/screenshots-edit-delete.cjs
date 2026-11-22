// Screenshots of the Queue screen with an edit form open and a delete confirmation.
// Usage: npm run build && node scripts/screenshots-edit-delete.cjs <outDir>
// Needs Playwright with Chromium already installed (require('playwright')).
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const dist = path.join(__dirname, "..", "dist");
const outDir = process.argv[2];
if (!outDir) {
  console.error("Usage: node scripts/screenshots-edit-delete.cjs <outDir>");
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

function serve() {
  const headers = readHeaders();
  const server = http.createServer((req, res) => {
    const name = req.url === "/" ? "/index.html" : req.url.split("?")[0];
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

const at = (y, mo, d, h, mi) => new Date(Date.UTC(y, mo - 1, d, h, mi)).getTime() / 1000;
const QUEUE = [
  { id: "a", video_key: "videos/a.mp4", title: "Sunday reel: three tips for tidy edits", caption: "Tips", platforms: ["instagram", "youtube"], slot_at: at(2099, 3, 5, 21, 30), status: "ready" },
  { id: "b", video_key: "videos/b.mp4", title: "Behind the scenes", caption: "", platforms: ["youtube"], slot_at: at(2099, 3, 8, 1, 0), status: "needs_caption" },
  { id: "c", video_key: "videos/c.mp4", title: "Weekly wrap-up", caption: "Wrap", platforms: ["instagram"], slot_at: at(2099, 3, 12, 8, 15), status: "ready" },
];

// Brisbane (+10:00) pattern and the next free slots it gives.
const PATTERN = {
  utc_offset_minutes: 600,
  rules: [
    { weekday: "mon", time: "06:30" },
    { weekday: "wed", time: "19:00" },
    { weekday: "fri", time: "08:30" },
  ],
};
// 2099-03-04 is a Wednesday. Times below are the UTC instants of the local slots.
const NEXT_SLOTS = [at(2099, 3, 4, 9, 0), at(2099, 3, 5, 22, 30), at(2099, 3, 8, 20, 30), at(2099, 3, 11, 9, 0), at(2099, 3, 12, 22, 30)];

function json(route, body) {
  return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });
}

function fail(message) {
  console.error(message);
  process.exitCode = 1;
}

async function shoot(browser, base, label, viewport) {
  const ctx = await browser.newContext({ viewport, deviceScaleFactor: 2, timezoneId: "Australia/Sydney", locale: "en-AU" });
  const page = await ctx.newPage();
  page.on("console", (m) => m.type() === "error" && fail(`console error: ${m.text()}`));
  page.on("pageerror", (e) => fail(`page error: ${e.message}`));
  await page.route("**/api/queue", (r) => json(r, QUEUE));
  await page.route("**/api/slots/pattern", (r) => json(r, PATTERN));
  await page.route("**/api/slots/next?count=5", (r) => json(r, { slots: NEXT_SLOTS }));
  await page.goto(base);
  await page.getByText("Weekly wrap-up").waitFor();

  await page.getByRole("button", { name: "Edit Sunday reel: three tips for tidy edits" }).click();
  await page.getByRole("radio", { name: "Fri 06/03/2099 08:30" }).waitFor();
  await page.mouse.move(0, 0);
  await page.screenshot({ path: path.join(outDir, `queue-edit-${label}.png`), fullPage: true });
  await page.getByRole("button", { name: "Cancel" }).click();

  await page.getByRole("button", { name: "Delete Weekly wrap-up" }).click();
  await page.getByRole("button", { name: "Delete post" }).waitFor();
  await page.mouse.move(0, 0);
  await page.screenshot({ path: path.join(outDir, `queue-delete-confirm-${label}.png`), fullPage: true });
  await ctx.close();
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
