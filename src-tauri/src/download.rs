//! 视频下载：把当前播放的 HLS 地址（含 AES-128 加密分片）合并成一个本地文件。
//!
//! 目录与权限约定：
//! - 桌面端由用户在「设置 → 下载」里选保存目录；写入前先探测一次可写性，
//!   落盘路径为 `<剧集名>/<集数>.ts|mp4`。目录不可写（如 Program Files、系统盘根）
//!   时直接返回可读的失败原因，不做静默降级。
//! - 移动端受系统沙箱限制，不能任意选外部目录，固定落在应用私有目录，因此
//!   `Settings::editable` 为 false，前端不再提供「更改」入口。
//! - 浏览目录用自带的 `dir_roots` / `dir_list`（只列子目录名），不引入额外的
//!   文件对话框依赖；任何会写盘的动作都先做可写探测。

use crate::net::{base as api_base, fetch_bytes, get};
use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes128;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;

/// 取消时统一返回这个标记，避免把「用户取消」当成错误弹提示。
const CANCELED: &str = "已取消下载";
/// 拿到的不是 m3u8：调用方据此退回「直链下载」。
const NOT_PLAYLIST: &str = "不是 HLS 播放列表";
const PART_SUFFIX: &str = ".hghub-part";

/* ───────────────────────── 状态 ───────────────────────── */

struct Store {
    /// 应用数据目录（download.json 落在这里）
    app_dir: Option<PathBuf>,
    /// 用户自定义的保存目录；None 表示用系统默认
    dir: Option<PathBuf>,
}

static STORE: Lazy<Mutex<Store>> = Lazy::new(|| Mutex::new(Store {
    app_dir: None,
    dir: None,
}));
static JOBS: Lazy<Mutex<HashMap<String, Status>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static CANCELS: Lazy<Mutex<HashMap<String, Arc<AtomicBool>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static JOB_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Clone)]
pub struct Settings {
    /// 当前生效目录
    pub dir: String,
    /// 默认目录（用于「恢复默认」时展示）
    pub default_dir: String,
    /// 是否允许用户自选目录（移动端为 false）
    pub editable: bool,
}

#[derive(Serialize, Clone)]
pub struct Status {
    pub id: String,
    /// preparing / downloading / done / error / canceled
    pub status: String,
    pub title: String,
    pub ep: u32,
    /// 已完成分片数
    pub done: u32,
    /// 总分片数（0 表示未知，例如直链 mp4）
    pub total: u32,
    pub bytes: u64,
    pub path: String,
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct DirListing {
    pub path: String,
    pub parent: Option<String>,
    pub dirs: Vec<DirEntry>,
}

fn is_mobile() -> bool {
    cfg!(any(target_os = "android", target_os = "ios"))
}

fn set_status(id: &str, f: impl FnOnce(&mut Status)) {
    if let Ok(mut jobs) = JOBS.lock() {
        if let Some(status) = jobs.get_mut(id) {
            f(status);
        }
    }
}

/* ───────────────────────── 设置持久化 ───────────────────────── */

/// 与 lines / playback_log 一致：启动时把应用数据目录交给本模块。
pub fn bind(dir: &Path) {
    if let Ok(mut store) = STORE.lock() {
        store.app_dir = Some(dir.to_path_buf());
        store.dir = fs::read_to_string(dir.join("download.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["dir"].as_str().map(PathBuf::from))
            .filter(|p| !p.as_os_str().is_empty());
    }
}

fn persist(dir: Option<&Path>) -> Result<(), String> {
    let app_dir = STORE
        .lock()
        .map_err(|_| "下载设置读取失败")?
        .app_dir
        .clone()
        .ok_or("设置目录尚未初始化")?;
    let payload = match dir {
        Some(d) => serde_json::json!({ "dir": d.display().to_string() }),
        None => serde_json::json!({}),
    };
    let temp = app_dir.join("download.json.tmp");
    fs::write(&temp, payload.to_string()).map_err(|e| format!("保存下载设置失败：{e}"))?;
    fs::rename(&temp, app_dir.join("download.json"))
        .map_err(|e| format!("保存下载设置失败：{e}"))?;
    Ok(())
}

fn default_dir(app: &tauri::AppHandle) -> PathBuf {
    if is_mobile() {
        return app
            .path()
            .app_data_dir()
            .map(|d| d.join("downloads"))
            .unwrap_or_else(|_| PathBuf::from("downloads"));
    }
    let base = app
        .path()
        .download_dir()
        .ok()
        .or_else(|| app.path().video_dir().ok())
        .or_else(|| app.path().home_dir().ok())
        .unwrap_or_else(std::env::temp_dir);
    base.join("HGhub")
}

fn effective_dir(app: &tauri::AppHandle) -> PathBuf {
    if is_mobile() {
        return default_dir(app);
    }
    STORE
        .lock()
        .ok()
        .and_then(|s| s.dir.clone())
        .unwrap_or_else(|| default_dir(app))
}

/// 目录参数校验：必须是绝对路径，不允许控制字符（也会被 Windows API 拒绝）。
fn normalize_dir(raw: &str) -> Result<PathBuf, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("目录不能为空".into());
    }
    if text.chars().any(char::is_control) {
        return Err("目录包含不可见字符".into());
    }
    if text.len() > 512 {
        return Err("目录过长".into());
    }
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err("请提供完整的绝对路径".into());
    }
    Ok(path)
}

/// 真正写一次探针文件：只有这里成功，才认为用户有写权限。
fn ensure_writable(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("无法创建目录 {}：{e}", dir.display()))?;
    let probe = dir.join(".hghub-write-test");
    match fs::write(&probe, b"ok") {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            Ok(())
        }
        Err(e) => Err(format!(
            "该文件夹不可写入（{e}）；请换一个位置，或以管理员身份运行后重试"
        )),
    }
}

/* ───────────────────────── 目录浏览（选文件夹用） ───────────────────────── */

fn push_dir(items: &mut Vec<DirEntry>, name: &str, path: Option<PathBuf>) {
    if let Some(p) = path {
        let text = p.display().to_string();
        if p.is_dir() && !items.iter().any(|i| i.path == text) {
            items.push(DirEntry {
                name: name.to_string(),
                path: text,
            });
        }
    }
}

#[tauri::command]
pub fn dir_roots(app: tauri::AppHandle) -> Vec<DirEntry> {
    let mut items: Vec<DirEntry> = Vec::new();
    push_dir(&mut items, "下载", app.path().download_dir().ok());
    push_dir(&mut items, "视频", app.path().video_dir().ok());
    push_dir(&mut items, "桌面", app.path().desktop_dir().ok());
    push_dir(&mut items, "文档", app.path().document_dir().ok());
    push_dir(&mut items, "主目录", app.path().home_dir().ok());
    #[cfg(target_os = "windows")]
    {
        for letter in b'C'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            let path = PathBuf::from(&root);
            if path.is_dir() {
                push_dir(&mut items, &root, Some(path));
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        push_dir(&mut items, "根目录", Some(PathBuf::from("/")));
    }
    items
}

#[tauri::command]
pub fn dir_list(app: tauri::AppHandle, path: Option<String>) -> Result<DirListing, String> {
    let dir = match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => normalize_dir(&p)?,
        None => effective_dir(&app),
    };
    // 默认目录（下载/HGhub）第一次使用前并不存在：向上找到最近的已存在祖先，
    // 用户可以在界面里点进来或直接新建，避免一打开选择器就报错。
    let mut dir = dir;
    for _ in 0..32 {
        if dir.is_dir() {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent.to_path_buf(),
            None => break,
        }
    }
    let mut dirs: Vec<DirEntry> = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|e| format!("无法读取 {}：{e}", dir.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let child = entry.path();
        // is_dir() 会跟随符号链接；坏链接直接跳过，避免点进去报错。
        if !child.is_dir() {
            continue;
        }
        dirs.push(DirEntry {
            name,
            path: child.display().to_string(),
        });
    }
    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    dirs.truncate(300);
    Ok(DirListing {
        path: dir.display().to_string(),
        parent: dir.parent().map(|p| p.display().to_string()),
        dirs,
    })
}

/* ───────────────────────── 命令：设置 ───────────────────────── */

/// 在选择器里新建一个子文件夹（省得用户切到资源管理器再回来）。
#[tauri::command]
pub fn dir_create(parent: String, name: String) -> Result<String, String> {
    let base = normalize_dir(&parent)?;
    let raw = name.trim();
    if raw.is_empty() {
        return Err("文件夹名不能为空".into());
    }
    // 全是非法字符时不要悄悄变成「未命名剧集」。
    if !raw
        .chars()
        .any(|c| !c.is_control() && !"/\\:*?\"<>|".contains(c))
    {
        return Err("文件夹名不合法".into());
    }
    let leaf = safe_name(raw);
    let target = base.join(leaf);
    if target.exists() {
        return Err("同名文件夹已存在".into());
    }
    fs::create_dir_all(&target).map_err(|e| format!("无法创建文件夹：{e}"))?;
    Ok(target.display().to_string())
}

#[tauri::command]
pub fn download_settings(app: tauri::AppHandle) -> Result<Settings, String> {
    let fallback = default_dir(&app);
    let custom = STORE.lock().map_err(|_| "下载设置读取失败")?.dir.clone();
    let dir = if is_mobile() {
        fallback.clone()
    } else {
        custom.unwrap_or_else(|| fallback.clone())
    };
    Ok(Settings {
        dir: dir.display().to_string(),
        default_dir: fallback.display().to_string(),
        editable: !is_mobile(),
    })
}

#[tauri::command]
pub fn download_dir_set(app: tauri::AppHandle, dir: String) -> Result<Settings, String> {
    if is_mobile() {
        return Err("移动端暂不支持自定义保存目录".into());
    }
    let path = normalize_dir(&dir)?;
    ensure_writable(&path)?;
    persist(Some(path.as_path()))?;
    if let Ok(mut store) = STORE.lock() {
        store.dir = Some(path);
    }
    download_settings(app)
}

#[tauri::command]
pub fn download_dir_reset(app: tauri::AppHandle) -> Result<Settings, String> {
    if is_mobile() {
        return Err("移动端暂不支持自定义保存目录".into());
    }
    persist(None)?;
    if let Ok(mut store) = STORE.lock() {
        store.dir = None;
    }
    download_settings(app)
}

/// 打开下载目录（桌面端）。移动端由用户在系统文件管理器里找。
#[tauri::command]
pub fn download_open_dir(app: tauri::AppHandle, dir: Option<String>) -> Result<(), String> {
    if is_mobile() {
        return Err("请用手机文件管理器查看下载内容".into());
    }
    let target = match dir.filter(|d| !d.trim().is_empty()) {
        Some(d) => PathBuf::from(d.trim()),
        None => effective_dir(&app),
    };
    if !target.exists() {
        return Err(format!("目录不存在：{}", target.display()));
    }
    app.opener()
        .open_path(target.display().to_string(), None::<&str>)
        .map_err(|e| format!("无法打开文件夹：{e}"))
}

/* ───────────────────────── 命令：下载任务 ───────────────────────── */

#[tauri::command]
pub async fn download_start(
    app: tauri::AppHandle,
    url: String,
    title: String,
    ep: u32,
) -> Result<String, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("播放地址为空".into());
    }
    if !matches!(ep, 1..=9999) {
        return Err("集数无效".into());
    }
    let id = format!("d{}", JOB_SEQ.fetch_add(1, Ordering::Relaxed) + 1);
    let root = effective_dir(&app);
    let cancel = Arc::new(AtomicBool::new(false));

    if let Ok(mut jobs) = JOBS.lock() {
        // 只保留仍在跑的任务，避免长时间使用后无限堆积。
        if jobs.len() > 48 {
            jobs.retain(|_, s| matches!(s.status.as_str(), "preparing" | "downloading"));
        }
        jobs.insert(
            id.clone(),
            Status {
                id: id.clone(),
                status: "preparing".into(),
                title: title.clone(),
                ep,
                done: 0,
                total: 0,
                bytes: 0,
                path: String::new(),
                error: None,
            },
        );
    }
    if let Ok(mut cancels) = CANCELS.lock() {
        cancels.insert(id.clone(), cancel.clone());
    }

    let job_id = id.clone();
    tauri::async_runtime::spawn(async move {
        run_job(job_id, url, title, ep, root, cancel).await;
    });
    Ok(id)
}

#[tauri::command]
pub fn download_status(id: String) -> Result<Option<Status>, String> {
    Ok(JOBS.lock().map_err(|_| "下载状态读取失败")?.get(&id).cloned())
}

#[tauri::command]
pub fn download_cancel(id: String) -> Result<bool, String> {
    let active = JOBS
        .lock()
        .map_err(|_| "下载状态读取失败")?
        .get(&id)
        .map(|s| matches!(s.status.as_str(), "preparing" | "downloading"))
        .unwrap_or(false);
    if active {
        if let Some(flag) = CANCELS.lock().map_err(|_| "下载状态读取失败")?.get(&id) {
            flag.store(true, Ordering::Relaxed);
        }
    }
    Ok(active)
}

async fn run_job(
    id: String,
    url: String,
    title: String,
    ep: u32,
    root: PathBuf,
    cancel: Arc<AtomicBool>,
) {
    let outcome = download_episode(&id, &url, &title, ep, &root, &cancel).await;
    match outcome {
        Ok(path) => {
            let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            set_status(&id, |s| {
                s.status = "done".into();
                s.path = path.display().to_string();
                s.bytes = bytes;
                s.error = None;
                if s.total > 0 {
                    s.done = s.total;
                }
            });
        }
        Err(e) => {
            let canceled = e == CANCELED;
            set_status(&id, |s| {
                s.status = if canceled { "canceled".into() } else { "error".into() };
                s.error = Some(e);
            });
        }
    }
}

/* ───────────────────────── 下载主体 ───────────────────────── */

async fn download_episode(
    id: &str,
    url: &str,
    title: &str,
    ep: u32,
    root: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let dir = root.join(safe_name(title));
    fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "无法创建文件夹 {}：{e}；请在设置里换一个可写入的目录",
            dir.display()
        )
    })?;

    // 目录建好后再探一次：用户可能在设置之后把目录删了 / 改了权限。
    let probe = dir.join(".hghub-write-test");
    fs::write(&probe, b"ok").map_err(|e| format!("该文件夹不可写入（{e}），请在设置里更换保存位置"))?;
    let _ = fs::remove_file(&probe);

    if looks_like_playlist(url) {
        return download_hls(id, url, ep, &dir, cancel).await;
    }
    // 后缀看不出类型（部分 CDN 用 /play 这类地址）：先按 HLS 试，
    // 明确「不是播放列表」再退回整文件直链下载，避免把 mp4 当列表解析。
    match download_hls(id, url, ep, &dir, cancel).await {
        Ok(path) => Ok(path),
        Err(e) if e == NOT_PLAYLIST => download_direct(id, url, ep, &dir, cancel).await,
        Err(e) => Err(e),
    }
}

fn looks_like_playlist(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.to_ascii_lowercase().ends_with(".m3u8")
}

fn io_err(e: std::io::Error) -> String {
    format!("写入文件失败：{e}")
}

async fn download_direct(
    id: &str,
    url: &str,
    ep: u32,
    dir: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let ext = ext_from_url(url);
    let part = dir.join(format!("{ep:02}.{ext}{}", PART_SUFFIX));
    let final_path = dir.join(format!("{ep:02}.{ext}"));
    set_status(id, |s| {
        s.status = "downloading".into();
        s.total = 0;
    });

    let referer = format!("{}/", api_base());
    let mut resp = get(url, Duration::from_secs(30), None, Some(&referer)).await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut written: u64 = 0;
    let outcome = async {
        let mut file = File::create(&part).map_err(io_err)?;
        while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
            if cancel.load(Ordering::Relaxed) {
                return Err(CANCELED.to_string());
            }
            file.write_all(&chunk).map_err(io_err)?;
            written += chunk.len() as u64;
            set_status(id, |s| {
                s.bytes = written;
                if total > 0 {
                    // 直链用字节进度映射到 0..=1000 的「分片」刻度，前端只关心百分比。
                    s.total = 1000;
                    s.done = ((written.min(total) * 1000) / total.max(1)) as u32;
                }
            });
        }
        file.flush().map_err(io_err)?;
        Ok::<(), String>(())
    }
    .await;

    match outcome {
        Ok(()) => {
            fs::rename(&part, &final_path)
                .or_else(|_| {
                    fs::copy(&part, &final_path)?;
                    fs::remove_file(&part)
                })
                .map_err(io_err)?;
            Ok(final_path)
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

fn ext_from_url(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    for (suffix, ext) in [
        (".mp4", "mp4"),
        (".m4v", "mp4"),
        (".mkv", "mkv"),
        (".webm", "webm"),
        (".mov", "mov"),
        (".flv", "flv"),
        (".ts", "ts"),
    ] {
        if path.ends_with(suffix) {
            return ext;
        }
    }
    "mp4"
}

/// HLS：拉主清单 → 选最高码率 → 合并全部分片（AES-128 就地解密）。
async fn download_hls(
    id: &str,
    url: &str,
    ep: u32,
    dir: &Path,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let (playlist_url, text) = resolve_playlist(url).await?;
    if cancel.load(Ordering::Relaxed) {
        return Err(CANCELED.into());
    }
    let parsed = parse_media(&text, &playlist_url)?;
    let ext = if parsed.init.is_some() { "mp4" } else { "ts" };
    let part = dir.join(format!("{ep:02}.{ext}{}", PART_SUFFIX));
    let final_path = dir.join(format!("{ep:02}.{ext}"));

    set_status(id, |s| {
        s.status = "downloading".into();
        s.total = parsed.segments.len() as u32;
        s.done = 0;
    });

    let outcome = write_hls(id, &parsed, &part, cancel).await;
    match outcome {
        Ok(()) => {
            fs::rename(&part, &final_path)
                .or_else(|_| {
                    fs::copy(&part, &final_path)?;
                    fs::remove_file(&part)
                })
                .map_err(io_err)?;
            Ok(final_path)
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

async fn write_hls(
    id: &str,
    parsed: &Parsed,
    part: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut file = std::io::BufWriter::new(File::create(part).map_err(io_err)?);
    let mut keys: HashMap<String, Arc<Vec<u8>>> = HashMap::new();
    let mut written: u64 = 0;

    if let Some(map) = &parsed.init {
        let (_, bytes) = fetch_bytes(map).await?;
        file.write_all(&bytes).map_err(io_err)?;
        written += bytes.len() as u64;
    }

    for (index, segment) in parsed.segments.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELED.into());
        }
        let (_, bytes) = fetch_bytes(&segment.url).await?;
        let data = match &segment.key {
            Some(key) => {
                let key_bytes = match keys.get(&key.uri) {
                    Some(hit) => hit.clone(),
                    None => {
                        let (_, raw) = fetch_bytes(&key.uri).await?;
                        if raw.len() != 16 {
                            return Err("解密密钥长度异常，无法合并分片".into());
                        }
                        let arc = Arc::new(raw);
                        keys.insert(key.uri.clone(), arc.clone());
                        arc
                    }
                };
                let iv = key.iv.unwrap_or_else(|| default_iv(segment.seq));
                aes128_cbc_decrypt(&key_bytes, &iv, &bytes)?
            }
            None => bytes,
        };
        file.write_all(&data).map_err(io_err)?;
        written += data.len() as u64;
        set_status(id, |s| {
            s.done = index as u32 + 1;
            s.bytes = written;
        });
    }
    file.flush().map_err(io_err)?;
    Ok(())
}

/* ───────────────────────── HLS 解析 ───────────────────────── */

#[derive(Clone)]
struct Key {
    uri: String,
    iv: Option<[u8; 16]>,
}

struct Segment {
    url: String,
    key: Option<Key>,
    seq: u64,
}

struct Parsed {
    segments: Vec<Segment>,
    init: Option<String>,
}

fn default_iv(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

fn parse_iv(raw: &str) -> Option<[u8; 16]> {
    let hex = raw.trim();
    let hex = hex
        .strip_prefix("0x")
        .or_else(|| hex.strip_prefix("0X"))
        .unwrap_or(hex);
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// 属性列表：引号内的逗号不切分（与 media.rs 的改写逻辑保持一致）。
fn parse_attrs(rest: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut quoted = false;
    let mut start = 0;
    let mut parts: Vec<&str> = Vec::new();
    for (i, ch) in rest.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        }
        if ch == ',' && !quoted {
            parts.push(&rest[start..i]);
            start = i + 1;
        }
    }
    parts.push(&rest[start..]);
    for part in parts {
        if let Some((key, value)) = part.split_once('=') {
            out.insert(
                key.trim().to_ascii_uppercase(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    out
}

fn absolutize(base: &str, target: &str) -> String {
    if target.starts_with("http://") || target.starts_with("https://") {
        return target.to_string();
    }
    if let Ok(b) = url::Url::parse(base) {
        if let Ok(joined) = b.join(target) {
            return joined.to_string();
        }
    }
    target.to_string()
}

/// 主清单里挑最高码率的子清单；不是主清单时返回 None。
fn best_variant(text: &str) -> Option<String> {
    let mut best: Option<(u64, String)> = None;
    let mut pending: Option<u64> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = parse_attrs(rest);
            let bandwidth = attrs
                .get("BANDWIDTH")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
            pending = Some(bandwidth);
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(bandwidth) = pending.take() {
            if best.as_ref().map(|(b, _)| bandwidth >= *b).unwrap_or(true) {
                best = Some((bandwidth, line.to_string()));
            }
        }
    }
    best.map(|(_, uri)| uri)
}

async fn resolve_playlist(url: &str) -> Result<(String, String), String> {
    let mut current = url.to_string();
    for _ in 0..3 {
        let (_, bytes) = fetch_bytes(&current).await?;
        let text = String::from_utf8(bytes).map_err(|_| "播放列表编码异常")?;
        let text = text.trim_start_matches('\u{feff}').to_string();
        if !text.starts_with("#EXTM3U") {
            return Err(NOT_PLAYLIST.into());
        }
        let is_master = text
            .lines()
            .any(|l| l.trim_start().starts_with("#EXT-X-STREAM-INF"));
        if is_master {
            let next = best_variant(&text).ok_or("无法从主播放列表选择清晰度")?;
            current = absolutize(&current, &next);
            continue;
        }
        return Ok((current.clone(), text));
    }
    Err("播放列表层级过深".into())
}

fn parse_media(text: &str, base: &str) -> Result<Parsed, String> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut key: Option<Key> = None;
    let mut init: Option<String> = None;
    let mut seq: u64 = 0;
    let mut pending = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = parse_attrs(rest);
            let method = attrs
                .get("METHOD")
                .cloned()
                .unwrap_or_default()
                .to_ascii_uppercase();
            if method == "NONE" {
                key = None;
            } else if method == "AES-128" {
                let uri = attrs.get("URI").ok_or("密钥地址缺失")?;
                key = Some(Key {
                    uri: absolutize(base, uri),
                    iv: attrs.get("IV").and_then(|v| parse_iv(v)),
                });
            } else {
                return Err(format!("暂不支持的分片加密方式：{method}"));
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MAP:") {
            init = parse_attrs(rest)
                .get("URI")
                .map(|uri| absolutize(base, uri));
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            seq = rest.trim().parse().unwrap_or(0);
            continue;
        }
        if line.starts_with("#EXTINF") {
            pending = true;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if !pending {
            continue;
        }
        segments.push(Segment {
            url: absolutize(base, line),
            key: key.clone(),
            seq,
        });
        seq += 1;
        pending = false;
    }

    if segments.is_empty() {
        return Err("播放列表里没有可下载的分片".into());
    }
    Ok(Parsed { segments, init })
}

/* ───────────────────────── AES-128-CBC ───────────────────────── */

fn aes128_cbc_decrypt(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, String> {
    if data.is_empty() || data.len() % 16 != 0 {
        return Err("分片长度不是 AES 分组长度".into());
    }
    let cipher = Aes128::new_from_slice(key).map_err(|_| "解密密钥长度不正确")?;
    let mut prev = *iv;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut ciphertext = [0u8; 16];
        ciphertext.copy_from_slice(chunk);
        let mut block = ciphertext;
        cipher.decrypt_block((&mut block).into());
        for i in 0..16 {
            out.push(block[i] ^ prev[i]);
        }
        prev = ciphertext;
    }
    // 每个分片各自带 PKCS#7 填充，只在末尾剥一次。
    let pad = *out.last().unwrap_or(&0) as usize;
    if pad > 0 && pad <= 16 && out.len() >= pad && out[out.len() - pad..].iter().all(|b| *b as usize == pad)
    {
        out.truncate(out.len() - pad);
    }
    Ok(out)
}

/* ───────────────────────── 文件名 ───────────────────────── */

/// 去掉 Windows/macOS 不允许的字符；保留中文，超长截断到 60 字符。
fn safe_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_control() || "/\\:*?\"<>|".contains(ch) {
            continue;
        }
        out.push(ch);
    }
    let cleaned = out.trim().trim_matches('.').trim();
    let capped: String = cleaned.chars().take(60).collect();
    let mut name = capped.trim().to_string();
    if name.is_empty() {
        return "未命名剧集".into();
    }
    let upper = name.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit());
    if reserved {
        name = format!("_{name}");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_playlist_keeps_keys_sequence_and_relative_urls() {
        let text = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:4,\nseg-1.ts\n#EXTINF:4,\nseg-2.ts\n";
        let parsed = parse_media(text, "https://cdn.example/v/a.m3u8").unwrap();
        assert_eq!(parsed.segments.len(), 2);
        assert_eq!(parsed.segments[0].seq, 7);
        assert_eq!(parsed.segments[1].seq, 8);
        assert_eq!(parsed.segments[0].url, "https://cdn.example/v/seg-1.ts");
        assert_eq!(
            parsed.segments[0].key.as_ref().unwrap().uri,
            "https://cdn.example/v/key.bin"
        );
        assert!(parsed.init.is_none());
    }

    #[test]
    fn master_playlist_picks_highest_bandwidth() {
        let text = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2400000\nhigh.m3u8\n";
        assert_eq!(best_variant(text).as_deref(), Some("high.m3u8"));
    }

    #[test]
    fn iv_and_aes_roundtrip() {
        use aes::cipher::BlockEncrypt;

        assert_eq!(default_iv(1)[15], 1);
        assert_eq!(
            parse_iv("0x0000000000000000000000000000000f").unwrap()[15],
            15
        );
        assert!(parse_iv("0xzz").is_none());

        let key = [7u8; 16];
        let iv = default_iv(3);
        let plain = b"segment-payload".to_vec();
        let mut buf = plain.clone();
        let pad = 16 - plain.len() % 16;
        buf.extend(std::iter::repeat_n(pad as u8, pad));

        let cipher = Aes128::new_from_slice(&key).unwrap();
        let mut prev = iv;
        let mut encrypted = Vec::with_capacity(buf.len());
        for chunk in buf.chunks(16) {
            let mut block = [0u8; 16];
            block.copy_from_slice(chunk);
            for i in 0..16 {
                block[i] ^= prev[i];
            }
            let mut out = block;
            cipher.encrypt_block((&mut out).into());
            encrypted.extend_from_slice(&out);
            prev = out;
        }
        assert_eq!(aes128_cbc_decrypt(&key, &iv, &encrypted).unwrap(), plain);
    }

    #[test]
    fn folder_names_are_sanitized() {
        assert_eq!(safe_name("短剧:第/1集?"), "短剧第1集");
        assert_eq!(safe_name("   "), "未命名剧集");
        assert_eq!(safe_name("CON"), "_CON");
        assert_eq!(safe_name(&"长".repeat(80)).chars().count(), 60);
    }
}
