import { useCallback, useEffect, useState } from "react";
import { downloadApi, type DirEntry, type DirListing } from "../download";
import { Modal } from "./ui";
import { IBack, IChevR, IFolder, IInfo, IPlus } from "../icons";

/* 选择保存文件夹：自带目录浏览（不依赖系统文件对话框），
   只列子目录名，路径可手动粘贴；确认前由 Rust 侧做真实写入探测。 */
export default function FolderPicker({ open, value, onClose, onPick }: {
  open: boolean;
  value?: string;
  onClose: () => void;
  onPick: (dir: string) => Promise<void>;
}) {
  const [listing, setListing] = useState<DirListing | null>(null);
  const [roots, setRoots] = useState<DirEntry[]>([]);
  const [input, setInput] = useState("");
  const [newName, setNewName] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async (path?: string) => {
    setLoading(true);
    setError("");
    try {
      const next = await downloadApi.list(path);
      setListing(next);
      setInput(next.path);
    } catch (e) {
      setError((e as Error).message || "无法读取该目录");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    setError("");
    setSaving(false);
    void load(value || undefined);
    downloadApi.roots().then(setRoots).catch(() => setRoots([]));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const confirm = async () => {
    const target = (input || listing?.path || "").trim();
    if (!target) return;
    setSaving(true);
    setError("");
    try {
      await onPick(target);
    } catch (e) {
      setError((e as Error).message || "该文件夹不可用");
      setSaving(false);
    }
  };

  const createFolder = async () => {
    const name = newName.trim();
    if (!name || !listing) return;
    setError("");
    try {
      const created = await downloadApi.createDir(listing.path, name);
      setNewName("");
      await load(created);
    } catch (e) {
      setError((e as Error).message || "新建文件夹失败");
    }
  };

  return (
    <Modal open={open} onClose={onClose} width={460}>
      <div className="p-4">
        <div className="text-[14.5px] font-bold text-on">选择保存文件夹</div>
        <p className="text-[11px] text-mut mt-1 leading-relaxed">
          视频会保存到「该文件夹 / 剧集名 / 集数」；选定后会立即验证写入权限。
        </p>

        <div className="flex items-center gap-1.5 mt-3">
          <button onClick={() => listing?.parent && load(listing.parent)} disabled={!listing?.parent || loading}
            aria-label="上一级" className="w-8 h-8 shrink-0 rounded-(--r-ctl) bg-s4/60 text-on flex items-center justify-center pressable disabled:opacity-40">
            <IBack size={16} />
          </button>
          <input value={input} onChange={(e) => setInput(e.target.value)} spellCheck={false}
            onKeyDown={(e) => { if (e.key === "Enter") void load(input); }}
            aria-label="文件夹路径"
            className="flex-1 min-w-0 h-8 px-2.5 rounded-(--r-ctl) bg-s4/60 text-[11.5px] text-on outline-none focus:ring-1 focus:ring-primary/60" />
          <button onClick={() => void load(input)} disabled={loading}
            aria-label="前往" className="w-8 h-8 shrink-0 rounded-(--r-ctl) bg-s4/60 text-on flex items-center justify-center pressable disabled:opacity-40">
            <IChevR size={16} />
          </button>
        </div>

        {roots.length > 0 && (
          <div className="flex flex-wrap gap-1.5 mt-2">
            {roots.map((r) => (
              <button key={r.path} onClick={() => void load(r.path)} title={r.path}
                className="h-6 px-2 rounded-full bg-s4/50 text-[10.5px] text-mut hover:text-on pressable">
                {r.name}
              </button>
            ))}
          </div>
        )}

        <div className="mt-2.5 rounded-(--r-sm) bg-s2 max-h-[228px] scroll-y divide-y divide-s3">
          {loading && <div className="h-9 flex items-center gap-2 px-3 text-[11px] text-mut"><span className="w-3.5 h-3.5 spinner" />读取中…</div>}
          {!loading && listing?.dirs.length === 0 && (
            <div className="h-9 flex items-center px-3 text-[11px] text-dim">该目录下没有子文件夹</div>
          )}
          {!loading && listing?.dirs.map((d) => (
            <button key={d.path} onClick={() => void load(d.path)}
              className="w-full h-9 flex items-center gap-2 px-3 text-left pressable">
              <IFolder size={15} className="text-primary shrink-0" />
              <span className="flex-1 min-w-0 truncate text-[11.5px] text-on/85">{d.name}</span>
              <IChevR size={13} className="text-dim shrink-0" />
            </button>
          ))}
        </div>

        {error && <div role="alert" className="mt-2 text-[10.5px] text-danger leading-relaxed break-all">{error}</div>}

        <div className="flex items-center gap-1.5 mt-2">
          <input value={newName} onChange={(e) => setNewName(e.target.value)} spellCheck={false}
            placeholder="在当前目录新建文件夹" aria-label="新建文件夹名称"
            onKeyDown={(e) => { if (e.key === "Enter") void createFolder(); }}
            className="flex-1 min-w-0 h-8 px-2.5 rounded-(--r-ctl) bg-s4/40 text-[11.5px] text-on outline-none focus:ring-1 focus:ring-primary/60" />
          <button onClick={() => void createFolder()} disabled={!newName.trim() || !listing}
            aria-label="新建文件夹" title="新建文件夹"
            className="w-8 h-8 shrink-0 rounded-(--r-ctl) bg-s4/60 text-on flex items-center justify-center pressable disabled:opacity-40">
            <IPlus size={15} />
          </button>
        </div>

        <div className="flex gap-2 mt-3.5">
          <button onClick={onClose} className="flex-1 h-9 rounded-(--r-ctl) bg-s3 text-[12.5px] text-on/80 pressable">取消</button>
          <button onClick={() => void confirm()} disabled={saving || loading || !input.trim()}
            className="flex-[1.4] h-9 rounded-(--r-ctl) bg-primary text-[12.5px] font-semibold text-on-primary pressable disabled:opacity-60">
            {saving ? "验证中…" : "使用此文件夹"}
          </button>
        </div>

        <div className="flex items-center gap-1 mt-2 text-[10px] text-dim">
          <IInfo size={11} />
          <span>若提示不可写入，请换到「下载 / 视频 / 主目录」下的文件夹</span>
        </div>
      </div>
    </Modal>
  );
}
