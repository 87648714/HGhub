import { dirOf, downloadApi, fmtBytes, progressPercent, type DownloadStatus } from "../download";
import { useStore } from "../store";
import { ICheck, IClose, IFolder } from "../icons";

/* 下载进度胶囊：桌面停右上角（避让右下角操作栏），移动端停左下（避让右侧操作栏）；
   下载中可取消，完成后可直接打开文件夹。 */
export default function DownloadChip({ job, top, bottom, align = "right", onCancel, onDismiss }: {
  job: DownloadStatus;
  top?: number;
  bottom?: number;
  align?: "left" | "right";
  onCancel: () => void;
  onDismiss: () => void;
}) {
  const { toast } = useStore();
  const running = job.status === "preparing" || job.status === "downloading";
  const done = job.status === "done";
  const canceled = job.status === "canceled";
  const pct = progressPercent(job);

  const openFolder = () => {
    downloadApi.openDir(dirOf(job.path))
      .catch((e) => toast((e as Error).message || "无法打开文件夹", "info"));
  };

  const style = top !== undefined
    ? { top: `calc(var(--sat, 0px) + ${top}px)` }
    : { bottom: `calc(var(--sab, 0px) + ${bottom ?? 0}px)` };

  return (
    <div className={`absolute z-30 pointer-events-none ${align === "left" ? "left-3" : "right-3"}`} style={style}>
      <div className="rounded-(--r-md) elev-2 px-2.5 py-2 pointer-events-auto w-[228px]"
        style={{ background: "rgba(14,15,20,.86)", color: "#fff" }}>
        <div className="flex items-center gap-2">
          {running ? <span className="w-3.5 h-3.5 spinner shrink-0" />
            : done ? <span className="w-4 h-4 rounded-full bg-primary flex items-center justify-center shrink-0"><ICheck size={10} className="text-on-primary" /></span>
              : <span className="w-4 h-4 rounded-full flex items-center justify-center shrink-0" style={{ background: "rgba(255,255,255,.22)" }}><IClose size={9} /></span>}

          <div className="flex-1 min-w-0">
            <div className="text-[11px] font-semibold truncate">
              {job.status === "preparing" ? `准备下载 第 ${job.ep} 集`
                : running ? `下载中 第 ${job.ep} 集`
                  : done ? `已保存 第 ${job.ep} 集`
                    : canceled ? "已取消下载" : "下载失败"}
            </div>
            <div className="text-[9.5px] leading-tight mt-0.5 truncate" style={{ color: "rgba(255,255,255,.62)" }}>
              {running ? `${pct > 0 ? `${pct}% · ` : ""}${fmtBytes(job.bytes)}`
                : done ? fmtBytes(job.bytes)
                  : (job.error || (canceled ? "未写入文件" : "请稍后重试"))}
            </div>
          </div>

          {running && (
            <button onClick={onCancel} aria-label="取消下载" className="w-6 h-6 rounded-full flex items-center justify-center pressable"
              style={{ background: "rgba(255,255,255,.14)" }}><IClose size={11} /></button>
          )}
          {done && (
            <>
              <button onClick={openFolder} aria-label="打开文件夹" title="打开文件夹"
                className="w-6 h-6 rounded-full flex items-center justify-center pressable"
                style={{ background: "rgba(255,255,255,.14)" }}><IFolder size={12} /></button>
              <button onClick={onDismiss} aria-label="关闭提示" className="w-6 h-6 rounded-full flex items-center justify-center pressable"
                style={{ background: "rgba(255,255,255,.14)" }}><IClose size={11} /></button>
            </>
          )}
          {!running && !done && (
            <button onClick={onDismiss} aria-label="关闭提示" className="w-6 h-6 rounded-full flex items-center justify-center pressable"
              style={{ background: "rgba(255,255,255,.14)" }}><IClose size={11} /></button>
          )}
        </div>

        {running && (
          <div className="mt-1.5 h-[3px] rounded-full overflow-hidden" style={{ background: "rgba(255,255,255,.16)" }}>
            <div className="h-full rounded-full bg-primary transition-all duration-300" style={{ width: `${Math.max(4, pct)}%` }} />
          </div>
        )}
      </div>
    </div>
  );
}
