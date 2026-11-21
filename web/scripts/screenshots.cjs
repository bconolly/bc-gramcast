// Serves the built app (web/dist) against a mocked API and saves screenshots.
// Usage: npm run build && node scripts/screenshots.cjs [outDir]
// Needs Playwright with Chromium already installed (require('playwright')).
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const dist = path.join(__dirname, "..", "dist");
const outDir = process.argv[2] || "/mnt/project-files/gramcast/screenshots/";
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css" };

function serve() {
  const server = http.createServer((req, res) => {
    const name = req.url === "/" ? "/index.html" : req.url.split("?")[0];
    const file = path.join(dist, path.normalize(name));
    if (!file.startsWith(dist) || !fs.existsSync(file)) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { "Content-Type": TYPES[path.extname(file)] || "application/octet-stream" });
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

async function shoot(browser, base, label, viewport) {
  const ctx = await browser.newContext({ viewport, deviceScaleFactor: 2, timezoneId: "Australia/Sydney", locale: "en-AU" });
  const page = await ctx.newPage();
  await page.route("**/api/queue", (r) => r.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(QUEUE) }));
  await page.goto(base);
  await page.getByText("Weekly wrap-up").waitFor();
  await page.screenshot({ path: path.join(outDir, `queue-${label}.png`), fullPage: true });

  await page.getByRole("button", { name: "Upload" }).click();
  await page.getByLabel("Video (MP4)").setInputFiles({ name: "sunday-reel.mp4", mimeType: "video/mp4", buffer: Buffer.from("demo") });
  await page.getByLabel("Title").fill("Sunday reel: three tips for tidy edits");
  await page.getByLabel("Caption").fill("Three small habits that keep your edits tidy. Save this for later!");
  await page.getByLabel("Instagram").check();
  await page.getByLabel("YouTube").check();
  await page.getByLabel("When").fill("2099-03-06T08:30");
  await page.screenshot({ path: path.join(outDir, `upload-${label}.png`), fullPage: true });
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
