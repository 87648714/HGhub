import React from "react";
import { createRoot } from "react-dom/client";
import { ChannelPage } from "../src/pages/Category";
import { AppProvider } from "../src/store";
import "../src/index.css";

/* 「分类列表加载下一页不能重排」的回归页面。
 *
 * 关键点：mock 数据的**服务端顺序刻意与 id 顺序不一致**（上游真实行为是按 hot 降序，
 * id 完全乱序）。旧实现渲染前会 `[...items].sort((a,b) => b.id - a.id)`，于是第二页里
 * id 更大的条目会被插到第一页前面 —— scrollTop 没变，内容却整体下移。
 * 所以这里的第一页首项是 id=3（不是最大 id），第二页里带着 id=24。 */
const pageSize = 12;

const card = (id: number) => ({
  id, title: `视频 ${id}`, cover: "", tags: ["测试"], score: 0, hot: 1000 - id,
  episode_count: 10, total_episodes: 10, is_finished: true, is_original: 0,
  like_count: 0, comment_count: 0, collect_count: 0,
});

const PAGES: Record<string, number[][]> = {
  // 默认序（最新）：服务端顺序 ≠ id 序
  new: [
    [3, 1, 2, 6, 5, 9, 4, 7, 8, 12, 11, 10],
    [24, 13, 15, 14, 20, 16, 19, 17, 18, 23, 21, 22],
  ],
  // 热度序：用 1xx 段，便于断言「切换排序是整表替换而不是追加」
  hot: [
    [101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112],
    [113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124],
  ],
};

type Call = { page: number; size: number; sort?: string };
const calls: Call[] = [];
const pending = new Map<string, () => void>();

const makePage = (sort: string, page: number) => {
  const rows = PAGES[sort] || PAGES.new;
  const ids = rows[page - 1] || [];
  return {
    items: ids.map(card),
    pagination: { page, pages: rows.length, size: pageSize, total: rows.length * pageSize },
  };
};

const fixture = {
  /** 最近一次列表请求的参数，用来确认排序是走服务端的 */
  calls,
  last(): Call | undefined { return calls[calls.length - 1]; },
  release(sort: string, page: number) { pending.get(`${sort}:${page}`)?.(); },
  reset() { pending.clear(); },
};

Object.assign(window, {
  channelFixture: fixture,
  __TAURI_INTERNALS__: {
    invoke(cmd: string, args: Record<string, unknown> = {}) {
      if (cmd === "app_boot") return Promise.resolve({ user: null, logged_in: false, base: "https://fixture.invalid" });
      if (cmd === "debug_log") return Promise.resolve();
      if (cmd === "api_category") {
        const page = Number(args.page || 1);
        const sort = typeof args.sort === "string" ? args.sort : "";
        calls.push({ page, size: Number(args.size || 0), sort });
        const payload = makePage(sort, page);
        // 第 1 页立即返回；第 2 页由脚本手动放行，便于在追加前后各量一次
        if (page === 1) return new Promise((resolve) => setTimeout(() => resolve(payload), 50));
        return new Promise((resolve) => pending.set(`${sort}:${page}`, () => resolve(payload)));
      }
      return Promise.reject(`Unexpected IPC: ${cmd}`);
    },
  },
});

createRoot(document.querySelector("#root")!).render(
  <React.StrictMode>
    <AppProvider>
      <div style={{ position: "relative", width: "100%", height: "100%" }}>
        <ChannelPage cate="ai-duanju" name="回归频道" onBack={() => {}} wide={false} />
      </div>
    </AppProvider>
  </React.StrictMode>,
);
