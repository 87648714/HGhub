// 回归：分类列表加载下一页时只能在尾部追加，不能重排。
//
// 历史 bug：ChannelPage 渲染前对「全部已加载项」做 [...items].sort(b.id - a.id)，
// 而上游分页是按 hot 降序返回、id 完全乱序 —— 于是第二页里 id 更大的条目会被插到
// 第一页前面。scrollTop 没变，但看的内容整体下移，表现为「新加载的数据加在前面」。
//
// 这里 mock 的服务端顺序刻意与 id 序不一致：第一页首项是 id=3，第二页带 id=24。
// 旧实现会让「视频 24」跑到最前面，测试即失败。
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import { chromium } from "playwright-core";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = join(root, "test-results", "channel-scroll", new Date().toISOString().replace(/[:.]/g, "-"));
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

  /* 量：滚动位置、全部卡片顺序、视口内可见的标题 */
  const snap = () => page.evaluate(() => {
    const el = document.querySelector(".scroll-y");
    if (!el) return null;
    const box = el.getBoundingClientRect();
    const titles = [];
    const visible = [];
    for (const card of el.querySelectorAll("button")) {
      const label = card.querySelector(".clamp-2");
      const title = label ? label.textContent : null;
      titles.push(title);
      const rect = card.getBoundingClientRect();
      if (rect.bottom > box.top + 2 && rect.top < box.bottom - 2) visible.push(title);
    }
    return { top: el.scrollTop, height: el.scrollHeight, cards: titles.length, titles, visible };
  });

  await page.goto(`${origin}tests/channel-scroll.html`);
  await page.waitForFunction(() => document.querySelectorAll(".scroll-y button").length === 12);

  // 排序必须走服务端：不再有客户端重排
  const first = await page.evaluate(() => window.channelFixture.last());
  assert.equal(first.sort, "new", "首屏应当把 sort=new 交给后端，而不是本地排序");
  const initial = await snap();
  assert.equal(initial.titles[0], "视频 3", "首屏顺序应当与 mock 的服务端顺序一致");

  await page.evaluate(() => {
    const el = document.querySelector(".scroll-y");
    el.scrollTop = el.scrollHeight;
  });
  await page.waitForTimeout(120);

  const before = await snap();
  assert(before && before.visible.length > 0, "底部应当有可见卡片");

  // 触底后观察器会请求第 2 页；脚本手动放行
  await page.waitForFunction(
    () => { const c = window.channelFixture.last(); return c && c.page === 2 && c.sort === "new"; },
    null, { timeout: 5000 },
  );
  await page.evaluate(() => window.channelFixture.release("new", 2));
  await page.waitForFunction(() => document.querySelectorAll(".scroll-y button").length === 24);
  await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))));
  await page.waitForTimeout(120);

  const after = await snap();
  assert.equal(after.cards, 24, "第二页应当追加到 24 张");
  assert.equal(after.titles[0], "视频 3", "已有内容顺序不能被重排（首项仍是第一页首项）");
  assert.equal(after.titles[12], "视频 24", "第二页条目应当严格追加在末尾，而不是插到前面");
  assert.deepEqual(after.titles.slice(0, 12), initial.titles, "第一页的 12 项顺序必须原样保留");
  assert.equal(after.top, before.top, "追加后滚动位置必须保持不变");
  assert(after.height > before.height, "追加后内容应当变长");
  for (const title of before.visible) {
    assert(after.visible.includes(title), `追加前可见的「${title}」不应被移出视口`);
  }
  report.passed.push("ChannelPage: 第二页只在尾部追加，首项与滚动位置都不变");

  // 切换排序 = 整表替换（重新拉第 1 页），不是对已加载内容重排
  await page.evaluate(() => {
    const btn = [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === "最热");
    if (!btn) throw new Error("找不到「最热」按钮");
    btn.click();
  });
  // 先确认点击生效且请求真的发出去了，再看列表是否被替换
  await page.waitForFunction(
    () => { const c = window.channelFixture.last(); return !!c && c.sort === "hot"; },
    null, { timeout: 5000 },
  );
  const hotCall = await page.evaluate(() => window.channelFixture.last());
  assert.equal(hotCall.page, 1, "切换排序必须从第 1 页重新拉");
  await page.waitForFunction(() => document.querySelectorAll(".scroll-y button").length === 12);
  const afterSort = await snap();
  assert.equal(afterSort.cards, 12, "切换排序应当整表替换（回到 12 张），不能累加");
  assert.equal(afterSort.titles[0], "视频 101", "切换排序后应当显示新排序的第 1 页");

  assert.deepEqual(errors, [], "No page errors");
  assert.deepEqual(report.external, [], "No upstream traffic");
  report.passed.push("ChannelPage: 切换排序整表替换并重新请求 sort=hot 第 1 页");
  await page.screenshot({ path: join(output, "final.png"), fullPage: true });
  console.log(JSON.stringify({ ...report, before: { top: before.top, visible: before.visible }, after: { top: after.top, order: after.titles } }, null, 2));
} catch (error) {
  report.error = String(error.stack || error);
  report.calls = await page?.evaluate(() => window.channelFixture?.calls ?? null).catch(() => null);
  report.dom = await page?.evaluate(() => ({
    cards: document.querySelectorAll(".scroll-y button").length,
    sortButtons: [...document.querySelectorAll("button")].map((b) => b.textContent?.trim()).filter((t) => t === "最新" || t === "最热"),
  })).catch(() => null);
  await page?.screenshot({ path: join(output, "failure.png"), fullPage: true }).catch(() => {});
  console.error(report.error);
  process.exitCode = 1;
} finally {
  await writeFile(join(output, "report.json"), JSON.stringify(report, null, 2));
  await browser?.close();
  await vite?.close();
  console.log(`Artifacts: ${output}`);
}
