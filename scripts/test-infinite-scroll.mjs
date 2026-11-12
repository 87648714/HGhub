// 回归：分类列表加载下一页后，滚动位置与当前可见卡片都保持不变。
// 历史 bug：Masonry 用 CSS 多列（columns + column-fill:balance），追加分页时已有卡片会被
// 重新分配到各列 —— scrollTop 没变，内容却整体位移，表现为「加载完下一页就跳回上面」。
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { chromium } from "playwright-core";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = join(root, "test-results", "infinite-scroll", new Date().toISOString().replace(/[:.]/g, "-"));
await mkdir(output, { recursive: true });
const report = { transport: "local browser fixture; Tauri IPC mocked", passed: [], external: [] };
let vite, browser, page;
try {
  vite = await createServer({ root, server: { host: "127.0.0.1", port: 0, strictPort: false } });
  await vite.listen();
  const origin = vite.resolvedUrls.local[0];
  browser = await chromium.launch({ headless: true, executablePath: process.env.BROWSER_EXECUTABLE || undefined });
  page = await browser.newPage({ viewport: { width: 520, height: 820 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.route("**/*", (route) => {
    const url = route.request().url();
    if (new URL(url).origin === new URL(origin).origin) return route.continue();
    report.external.push(url);
    return route.abort();
  });

  /* 量：滚动位置 + 视口里看得见的卡片标题 */
  const snap = () => page.evaluate(() => {
    const el = document.querySelector(".scroll-y");
    if (!el) return null;
    const box = el.getBoundingClientRect();
    const visible = [];
    for (const card of el.querySelectorAll("button")) {
      const rect = card.getBoundingClientRect();
      if (rect.bottom > box.top + 2 && rect.top < box.bottom - 2) {
        const title = card.querySelector(".clamp-2");
        visible.push(title ? title.textContent : null);
      }
    }
    return { top: el.scrollTop, height: el.scrollHeight, cards: el.querySelectorAll("button").length, visible };
  });

  await page.goto(`${origin}tests/infinite-scroll.html`);
  await page.waitForFunction(() => window.scrollFixture.served === 1);
  await page.waitForFunction(() => document.querySelectorAll(".scroll-y button").length === 20);
  await page.evaluate(() => {
    const el = document.querySelector(".scroll-y");
    el.scrollTop = el.scrollHeight;
  });
  await page.waitForTimeout(120);

  const before = await snap();
  assert(before && before.visible.length > 0, "底部应当有可见卡片");

  // 触发续页（触底后观察器放行），再手动放行第二页数据
  await page.waitForFunction(() => window.scrollFixture.served === 2, null, { timeout: 5000 });
  await page.evaluate(() => window.scrollFixture.release(2));
  await page.waitForFunction(() => document.querySelectorAll(".scroll-y button").length === 40);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  await page.waitForTimeout(120);

  const after = await snap();
  assert.equal(after.top, before.top, "追加后滚动位置必须保持不变");
  assert(after.height > before.height, "追加后内容应当变长");
  for (const title of before.visible) {
    assert(after.visible.includes(title), `追加前可见的「${title}」不应被移出视口`);
  }
  assert.deepEqual(errors, [], "No page errors");
  assert.deepEqual(report.external, [], "No upstream traffic");
  report.passed.push("ChannelPage: appending a page keeps scrollTop and the visible cards in place");
  await page.screenshot({ path: join(output, "final.png"), fullPage: true });
  console.log(JSON.stringify({ ...report, before, after }, null, 2));
} catch (error) {
  report.error = String(error.stack || error);
  await page?.screenshot({ path: join(output, "failure.png"), fullPage: true }).catch(() => {});
  console.error(report.error);
  process.exitCode = 1;
} finally {
  await writeFile(join(output, "report.json"), JSON.stringify(report, null, 2));
  await browser?.close();
  await vite?.close();
  console.log(`Artifacts: ${output}`);
}
