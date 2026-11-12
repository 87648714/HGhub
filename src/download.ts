import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

/* 下载：目录设置在 Rust 侧持久化（download.json），下载本身也在 Rust 侧完成
   （HLS 合并 + AES-128 解密），前端只负责发起任务和轮询进度。 */

export type DownloadSettings = {
  /** 当前生效目录 */
  dir: string;
  /** 默认目录（下载目录/HGhub） */
  default_dir: string;
  /** 桌面端可自选目录，移动端受沙箱限制固定为应用目录 */
  editable: boolean;
};

export type DirEntry = { name: string; path: string };
export type DirListing = { path: string; parent: string | null; dirs: DirEntry[] };

export type DownloadStatus = {
  id: string;
  status: "preparing" | "downloading" | "done" | "error" | "canceled";
  title: string;
  ep: number;
  /** 已完成分片（直链下载时是 0..1000 的字节刻度） */
  done: number;
  /** 分片总数，0 表示未知 */
  total: number;
  bytes: number;
  path: string;
  error: string | null;
};

/** Tauri 的 invoke 失败时抛出的是字符串，这里统一成 Error，界面才能拿到原文提示。 */
async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    const msg = typeof e === "string" ? e : (e as Error | null)?.message;
    throw new Error(msg && msg.trim() ? msg : "操作失败，请重试");
  }
}

export const downloadApi = {
  settings: () => call<DownloadSettings>("download_settings"),
  setDir: (dir: string) => call<DownloadSettings>("download_dir_set", { dir }),
  resetDir: () => call<DownloadSettings>("download_dir_reset"),
  openDir: (dir?: string) => call<void>("download_open_dir", { dir: dir ?? null }),
  roots: () => call<DirEntry[]>("dir_roots"),
  list: (path?: string) => call<DirListing>("dir_list", { path: path ?? null }),
  createDir: (parent: string, name: string) => call<string>("dir_create", { parent, name }),
  start: (url: string, title: string, ep: number) =>
    call<string>("download_start", { url, title, ep }),
  status: (id: string) => call<DownloadStatus | null>("download_status", { id }),
  cancel: (id: string) => call<boolean>("download_cancel", { id }),
};

export const isDownloading = (s?: DownloadStatus | null) =>
  !!s && (s.status === "preparing" || s.status === "downloading");

export const progressPercent = (s?: DownloadStatus | null) =>
  s && s.total > 0 ? Math.min(100, Math.round((s.done / s.total) * 100)) : 0;

export const fmtBytes = (n: number) => {
  if (!n) return "—";
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} GB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(n / 1024))} KB`;
};

/** 文件所在目录（「打开文件夹」用） */
export const dirOf = (path: string) => {
  const i = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return i > 0 ? path.slice(0, i) : path;
};

/** 轮询下载进度：500ms 一次，任务结束自动停止；回调用 ref 持有，避免重复触发。 */
export function useDownloadJob(options?: {
  onDone?: (s: DownloadStatus) => void;
  onError?: (s: DownloadStatus) => void;
}) {
  const [job, setJob] = useState<DownloadStatus | null>(null);
  const doneRef = useRef(options?.onDone);
  const errorRef = useRef(options?.onError);
  doneRef.current = options?.onDone;
  errorRef.current = options?.onError;
  const settledRef = useRef("");

  const jobId = job?.id;
  const running = isDownloading(job);

  useEffect(() => {
    if (!jobId || !running) return undefined;
    let alive = true;
    const timer = window.setInterval(() => {
      downloadApi.status(jobId)
        .then((next) => { if (alive && next) setJob(next); })
        .catch(() => { /* 单次读取失败不打断轮询 */ });
    }, 500);
    return () => { alive = false; window.clearInterval(timer); };
  }, [jobId, running]);

  useEffect(() => {
    if (!job) return;
    const settled = job.status === "done" || job.status === "error" || job.status === "canceled";
    const key = `${job.id}:${job.status}`;
    if (!settled || settledRef.current === key) return;
    settledRef.current = key;
    if (job.status === "done") doneRef.current?.(job);
    else if (job.status === "error") errorRef.current?.(job);
  }, [job]);

  // 成功后自动收起胶囊，失败/取消留给用户自己关（要能看清原因）。
  useEffect(() => {
    if (job?.status !== "done") return undefined;
    const timer = window.setTimeout(() => setJob(null), 6000);
    return () => window.clearTimeout(timer);
  }, [job?.id, job?.status]);

  const start = useCallback(async (url: string, title: string, ep: number) => {
    const id = await downloadApi.start(url, title, ep);
    settledRef.current = "";
    setJob({
      id, status: "preparing", title, ep,
      done: 0, total: 0, bytes: 0, path: "", error: null,
    });
    return id;
  }, []);

  const cancel = useCallback(async () => {
    if (!job) return;
    try { await downloadApi.cancel(job.id); } catch { /* 任务可能刚好结束 */ }
  }, [job]);

  const dismiss = useCallback(() => setJob(null), []);

  return { job, running, start, cancel, dismiss };
}
