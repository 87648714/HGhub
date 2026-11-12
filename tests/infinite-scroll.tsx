import React from "react";
import { createRoot } from "react-dom/client";
import { ChannelPage } from "../src/pages/Category";
import { AppProvider } from "../src/store";
import "../src/index.css";

/* 「加载下一页后滚动位置不能变」的回归页面：
   只发两页数据，第二页由测试脚本手动放行，便于在追加前后各量一次。 */
const total = 40;
const pageSize = 20;

const makePage = (page: number) => ({
  items: Array.from({ length: Math.min(pageSize, total - (page - 1) * pageSize) }, (_, i) => {
    const id = total - ((page - 1) * pageSize + i);
    return {
      id, title: `视频 ${id}`, cover: "", tags: ["测试"], score: 0, hot: 0,
      episode_count: 10, total_episodes: 10, is_finished: true, is_original: 0,
      like_count: 0, comment_count: 0, collect_count: 0,
    };
  }),
  pagination: { page, pages: Math.ceil(total / pageSize), size: pageSize, total },
});

const pending = new Map<number, () => void>();
const fixture = {
  served: 0,
  release(page: number) { pending.get(page)?.(); },
  reset() { pending.clear(); },
};
Object.assign(window, {
  scrollFixture: fixture,
  __TAURI_INTERNALS__: {
    invoke(cmd: string, args: Record<string, unknown> = {}) {
      if (cmd === "app_boot") return Promise.resolve({ user: null, logged_in: false, base: "https://fixture.invalid" });
      if (cmd === "debug_log") return Promise.resolve();
      if (cmd === "api_category") {
        const page = Number(args.page || 1);
        fixture.served = page;
        if (page === 1) return new Promise((resolve) => setTimeout(() => resolve(makePage(1)), 60));
        return new Promise((resolve) => pending.set(page, () => resolve(makePage(page))));
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
