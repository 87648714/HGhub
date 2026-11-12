//! 线路（数据源）：固定线路 + 镜像线路。
//!
//! 上游只是域名在变，请求链路与数据模型完全一致，所以「切线路」等价于换 `net::base()`。
//!
//! 镜像清单有两个来源，按可靠性排序：
//!
//! 1. **站点官方地址清单** `GET /api/site/address-poster`（页面上的「保存回家的路」用的就是它）。
//!    它和业务接口同源，跟着当前线路走，大陆可直连的线路都能取到，还自带「大陆可访问 / 需梯子」标记。
//! 2. **地址发布页** <https://huangguoai.pages.dev/>（兜底）。页面本身是空壳，真正的清单在
//!    `publish.js` 里（`var urls` 域名数组 + `var subdomains` 子域数组）。
//!    `*.pages.dev` 在大陆被 DNS 污染到 31.13.x.x，只有挂代理才取得到，所以不能当主来源。
//!
//! 两个来源都取不到时，回退到上次成功缓存的清单（应用数据目录 `lines.json`），
//! 至少让用户还能手动挑一条能用的线路。
//!
//! 用户也可以手动加线路（`custom_lines.json`）。可用性判定统一打业务接口
//! `/api/site/address-poster`：首页 200 不算数，跳转页和发布页的 `/api/**` 全是 404。

use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// 固定线路：写死在客户端里，不依赖任何外部页面。
pub const FIXED: [(&str, &str); 2] = [
    ("固定线路 1", "https://huangguoai.com"),
    ("固定线路 2", "https://huangguoai.ai"),
];

/// 站点官方地址清单接口：与业务接口同源，不需要信封，普通 GET 即可。
const POSTER_PATH: &str = "/api/site/address-poster";
/// 地址发布页（兜底来源，大陆需代理）
const PUBLISH_PAGE: &str = "https://huangguoai.pages.dev/";
/// 大陆可直连的官方入口：当前线路取不到清单时用它们当探测源。
/// 清单接口只在站点本源上；`hgai13.com` 是「最新地址入口」跳转页，`/api/**` 恒为 404，不能当探测源。
const SEED_ORIGINS: [&str; 1] = ["https://ec2b.rlhjeyuvw.cc"];
/// 镜像列表上限（地址清单通常只有几条，子域 × 域名最多会到几十条）
const MIRROR_MAX: usize = 60;
/// 单条线路探测超时
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// 单个清单来源的超时（多源并发，取最先成功的那个）
const SOURCE_TIMEOUT: Duration = Duration::from_secs(7);
/// 地址发布页 / publish.js 的超时
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(9);

/// 上次成功清单的落盘位置，由 [`bind`] 在启动时绑定。
static CACHE_FILE: Lazy<RwLock<Option<PathBuf>>> = Lazy::new(|| RwLock::new(None));

/// 用户手动添加的线路（同样是纯 HTTPS origin），与其落盘位置。
static CUSTOM_FILE: Lazy<RwLock<Option<PathBuf>>> = Lazy::new(|| RwLock::new(None));
static CUSTOM: Lazy<RwLock<Vec<String>>> = Lazy::new(|| RwLock::new(Vec::new()));
/// 自定义条目上限：清单拉不到时靠它自救，但不能无限堆。
const CUSTOM_MAX: usize = 12;

/// 启动时绑定缓存路径（应用数据目录），与 `net::bind_base` 同一个目录。
pub fn bind(dir: &std::path::Path) {
    if let Ok(mut file) = CACHE_FILE.write() {
        *file = Some(dir.join("lines.json"));
    }
    let path = dir.join("custom_lines.json");
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Some(list) = parse_custom(&text) {
            if let Ok(mut cur) = CUSTOM.write() {
                *cur = list;
            }
        }
    }
    if let Ok(mut file) = CUSTOM_FILE.write() {
        *file = Some(path);
    }
}

pub fn fixed() -> Value {
    json!(FIXED
        .iter()
        .map(|(name, url)| json!({ "name": name, "url": url }))
        .collect::<Vec<_>>())
}

/* ───────────────────────── 自定义线路 ───────────────────────── */

pub fn custom() -> Value {
    let list = CUSTOM.read().map(|l| l.clone()).unwrap_or_default();
    json!(list
        .iter()
        .map(|url| {
            let host = url::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            json!({ "name": "自定义线路", "url": url, "host": host })
        })
        .collect::<Vec<_>>())
}

/// 添加自定义线路：只收纯 HTTPS origin，重复条目按成功处理。
/// 可用性不在这里判断，调用方应先跑一次 [`probe`]。
pub fn custom_add(raw: &str) -> Result<String, String> {
    let origin = crate::net::normalize_base(raw).ok_or_else(|| {
        "线路地址无效（需要纯 HTTPS 站点地址，如 https://example.com，不能带路径或参数）".to_string()
    })?;
    if FIXED.iter().any(|(_, url)| origin == *url) {
        return Err("这条已经内置在固定线路里".to_string());
    }
    let mut list = CUSTOM.write().map_err(|_| "自定义线路锁失败")?;
    if list.contains(&origin) {
        return Ok(origin);
    }
    if list.len() >= CUSTOM_MAX {
        return Err(format!("自定义线路最多 {CUSTOM_MAX} 条，请先移除不用的"));
    }
    list.push(origin.clone());
    save_custom(list.as_slice());
    Ok(origin)
}

pub fn custom_remove(raw: &str) -> Result<(), String> {
    let origin = crate::net::normalize_base(raw).ok_or("线路地址无效")?;
    let mut list = CUSTOM.write().map_err(|_| "自定义线路锁失败")?;
    list.retain(|url| url != &origin);
    save_custom(list.as_slice());
    Ok(())
}

fn save_custom(list: &[String]) {
    let Some(path) = CUSTOM_FILE.read().ok().and_then(|p| p.clone()) else {
        return;
    };
    let _ = std::fs::write(path, json!({ "items": list }).to_string());
}

/// 坏文件当没有；非 HTTPS、带路径的条目直接丢掉。
fn parse_custom(text: &str) -> Option<Vec<String>> {
    let value: Value = serde_json::from_str(text).ok()?;
    let mut out: Vec<String> = Vec::new();
    for raw in value.get("items")?.as_array()? {
        let Some(origin) = raw.as_str().and_then(crate::net::normalize_base) else {
            continue;
        };
        if !out.contains(&origin) {
            out.push(origin);
        }
    }
    Some(out)
}

/// 拉镜像线路：官方清单 → 地址发布页 → 上次缓存。只在用户点开「线路」时调用。
pub async fn mirrors() -> Result<Value, String> {
    let poster_err = match poster_all().await {
        Ok(items) => {
            let value = json!({ "source": POSTER_PATH, "items": items });
            save_cache(&value);
            return Ok(value);
        }
        Err(e) => e,
    };

    match publish_mirrors().await {
        Ok(items) => {
            let value = json!({ "source": PUBLISH_PAGE, "items": items });
            save_cache(&value);
            Ok(value)
        }
        // 全部失败：有缓存就先用缓存，让用户还能手动挑线路，而不是一个空列表。
        Err(e) => match load_cache() {
            Some(mut cached) => {
                cached["cached"] = json!(true);
                Ok(cached)
            }
            None => Err(format!("{poster_err}；地址发布页也不可达：{e}")),
        },
    }
}

/// 逐源并发请求官方清单，谁先成功用谁（各源内容一致，只是主机不同）。
async fn poster_all() -> Result<Vec<Value>, String> {
    let mut sources: Vec<String> = vec![
        crate::net::base(),
        FIXED[0].1.to_string(),
        FIXED[1].1.to_string(),
    ];
    sources.extend(SEED_ORIGINS.iter().map(|s| s.to_string()));
    let mut seen = std::collections::HashSet::new();
    sources.retain(|s| seen.insert(s.clone()));

    let mut set = tokio::task::JoinSet::new();
    for origin in sources {
        set.spawn(poster_from(origin));
    }
    let mut last = String::from("官方地址清单不可达");
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(items)) => return Ok(items),
            Ok(Err(e)) => last = e,
            Err(_) => {}
        }
    }
    Err(last)
}

async fn poster_from(origin: String) -> Result<Vec<Value>, String> {
    let url = format!("{}{POSTER_PATH}", origin.trim_end_matches('/'));
    let text = get_text_within(&url, SOURCE_TIMEOUT).await?;
    let value: Value = serde_json::from_str(&text).map_err(|_| "地址清单不是合法 JSON".to_string())?;
    let items = poster_items(&value);
    if items.is_empty() {
        return Err("地址清单里没有可用线路".to_string());
    }
    Ok(items)
}

/// 把 `data.config.addresses` 转成线路条目：按 `sort` 排序，跳过隐藏项和非公网 HTTP(S) 地址。
fn poster_items(value: &Value) -> Vec<Value> {
    let list = value
        .get("data")
        .and_then(|d| d.get("config"))
        .and_then(|c| c.get("addresses"))
        .and_then(Value::as_array);
    let Some(list) = list else {
        return Vec::new();
    };
    // `sort_by_key` 是稳定排序：同 sort 时保持接口顺序。
    let mut ordered: Vec<&Value> = list.iter().collect();
    ordered.sort_by_key(|a| a.get("sort").and_then(Value::as_i64).unwrap_or(i64::MAX));

    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in ordered {
        if item.get("hidden").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(raw) = item.get("url").and_then(Value::as_str) else {
            continue;
        };
        // 只收纯 HTTPS 站点地址：与 line_set 的约束保持一致，防止清单里混进内网或带路径的地址。
        let Some(origin) = crate::net::normalize_base(raw) else {
            continue;
        };
        if !seen.insert(origin.clone()) {
            continue;
        }
        let host = url::Url::parse(&origin)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        out.push(json!({
            "name": item.get("label").and_then(Value::as_str).unwrap_or("官方地址"),
            "url": origin,
            "host": host,
            "note": item.get("status_label").and_then(Value::as_str).unwrap_or(""),
        }));
        if out.len() >= MIRROR_MAX {
            break;
        }
    }
    out
}

/// 兜底：地址发布页 → publish.js → 子域 × 域名。
async fn publish_mirrors() -> Result<Vec<Value>, String> {
    let html = get_text_within(PUBLISH_PAGE, PUBLISH_TIMEOUT).await?;
    let js_rel = script_src(&html).unwrap_or_else(|| "publish.js".to_string());
    let js_url = absolute(PUBLISH_PAGE, &js_rel);
    let js = get_text_within(&js_url, PUBLISH_TIMEOUT).await?;

    let domains = js_array(&js, "var urls");
    let subs = js_array(&js, "var subdomains");
    if domains.is_empty() {
        return Err("地址发布页格式有变化：没找到镜像域名".to_string());
    }

    let mut items: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    'outer: for domain in domains {
        let domain = domain.trim_end_matches('/').to_string();
        if domain.is_empty() {
            continue;
        }
        let hosts: Vec<String> = if subs.is_empty() {
            vec![domain.clone()]
        } else {
            subs.iter().map(|s| format!("{s}.{domain}")).collect()
        };
        for host in hosts {
            if !seen.insert(host.clone()) {
                continue;
            }
            items.push(json!({ "url": format!("https://{host}"), "host": host }));
            if items.len() >= MIRROR_MAX {
                break 'outer;
            }
        }
    }
    if items.is_empty() {
        return Err("地址发布页里没有可用的镜像地址".to_string());
    }
    Ok(items)
}

fn save_cache(value: &Value) {
    let Some(path) = CACHE_FILE.read().ok().and_then(|p| p.clone()) else {
        return;
    };
    let _ = std::fs::write(path, value.to_string());
}

fn load_cache() -> Option<Value> {
    let path = CACHE_FILE.read().ok().and_then(|p| p.clone())?;
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    // 只认结构完好的非空清单，坏文件当没有。
    if value.get("items")?.as_array()?.is_empty() {
        return None;
    }
    Some(value)
}

/// 可达性探测（并发 6 条一组）。只报结果，不做自动切换。
pub async fn probe(urls: Vec<String>) -> Value {
    let mut out: Vec<Value> = Vec::new();
    for chunk in urls.chunks(6) {
        let mut handles = Vec::new();
        for u in chunk {
            let u = u.clone();
            handles.push(tokio::spawn(async move { probe_one(u).await }));
        }
        for h in handles {
            if let Ok(v) = h.await {
                out.push(v);
            }
        }
    }
    json!(out)
}

async fn probe_one(url: String) -> Value {
    let started = Instant::now();
    let res = business_check(&url).await;
    let ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(status) => json!({ "url": url, "ok": true, "status": status, "ms": ms }),
        Err(e) => json!({ "url": url, "ok": false, "ms": ms, "error": e }),
    }
}

/// 判定一条线路能否当业务源用：必须取得到站点的地址清单。
///
/// 不能只看首页状态码——「最新地址入口」这类跳转页和静态发布页首页都是 200，
/// 但 `/api/**` 全是 404，选中后必然加载失败（用户看到的就是「显示能连、加不上」）。
async fn business_check(origin: &str) -> Result<u16, String> {
    let target = format!("{}{POSTER_PATH}", origin.trim_end_matches('/'));
    let resp = crate::net::get(&target, PROBE_TIMEOUT, Some("application/json"), None).await?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(match status {
            403 | 404 => format!("不是业务线路（地址清单 HTTP {status}，多半是跳转页）"),
            500..=599 => format!("线路暂时不可用（HTTP {status}）"),
            _ => format!("HTTP {status}"),
        });
    }
    let text = resp.text().await.map_err(|e| crate::net::err_text(&e))?;
    check_poster_body(&text)?;
    Ok(status)
}

/// 地址清单必须是 JSON 且带 `data.config`；返回网页说明这一跳只是跳转页/发布页。
fn check_poster_body(body: &str) -> Result<(), String> {
    let value: Value = serde_json::from_str(body).map_err(|_| {
        "不是业务线路（返回网页而不是接口数据，多半是跳转页或发布页）".to_string()
    })?;
    if value.get("data").and_then(|d| d.get("config")).is_none() {
        return Err("不是业务线路（接口里没有地址清单配置）".to_string());
    }
    Ok(())
}

async fn get_text_within(url: &str, timeout: Duration) -> Result<String, String> {
    let resp = crate::net::get(url, timeout, None, None).await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.text().await.map_err(|e| crate::net::err_text(&e))
}

/// 从 HTML 里挑出脚本地址（发布页只有一个 `publish.js?<版本>`）。
fn script_src(html: &str) -> Option<String> {
    let mut rest = html;
    while let Some(i) = rest.find("src=\"") {
        let after = &rest[i + 5..];
        let end = after.find('"')?;
        let candidate = &after[..end];
        if candidate.contains(".js") {
            return Some(candidate.to_string());
        }
        rest = &after[end..];
    }
    None
}

fn absolute(base: &str, rel: &str) -> String {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    let origin = url::Url::parse(base)
        .ok()
        .map(|u| format!("{}://{}", u.scheme(), u.host_str().unwrap_or_default()));
    match origin {
        Some(o) if rel.starts_with('/') => format!("{o}{rel}"),
        Some(o) => format!("{o}/{rel}"),
        None => rel.to_string(),
    }
}

/// 取 JS 里 `var <name> = [ ... ]` 的字符串数组（去掉行尾 `//` 注释，但保留字符串里的 `//`）。
fn js_array(src: &str, decl: &str) -> Vec<String> {
    let Some(at) = src.find(decl) else {
        return Vec::new();
    };
    let rest = &src[at..];
    let Some(open) = rest.find('[') else {
        return Vec::new();
    };
    let Some(close) = rest[open..].find(']') else {
        return Vec::new();
    };
    strip_line_comments(&rest[open + 1..open + close])
        .split(',')
        .map(|raw| {
            raw.trim()
                .trim_matches(|c: char| c == '\'' || c == '"')
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// 去掉 `//` 行尾注释（引号内的 `//` 是 URL，不能动）。
fn strip_line_comments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut quote: Option<char> = None;
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                out.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                    out.push(c);
                } else if c == '/' && chars.peek() == Some(&'/') {
                    for n in chars.by_ref() {
                        if n == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
var subdomains = [
    'thu',  // 清华
    'pku',  // 北大
];
var urls=[
	'xxbgwypx.cc/',
];
var otherUrls = [
	'https://huangguoai.pages.dev',
];
var foreverUrls = [
	  'https://huangguoai.com',
	JumpPage,
	'https://huangguoai.ai',
];
"#;

    /// 线上 `/api/site/address-poster` 的裁剪样本（2026-11 抓取）。
    const POSTER: &str = r#"{
      "status": 1,
      "data": { "config": { "addresses": [
        {"label":"永久地址发布页","url":"https://huangguoai.pages.dev","status":"available","status_label":"大陆可访问","hidden":false,"sort":3},
        {"label":"最新地址","url":"https://ec2b.rlhjeyuvw.cc","status":"available","status_label":"大陆可访问","hidden":false,"sort":1},
        {"label":"永久域名","url":"https://huangguoai.com","status":"restricted","status_label":"需梯子","hidden":false,"sort":4},
        {"label":"隐藏项","url":"https://hidden.example","hidden":true,"sort":9},
        {"label":"内网项","url":"http://127.0.0.1:8080/path","status":"available","sort":10},
        {"label":"重复项","url":"https://ec2b.rlhjeyuvw.cc/","sort":11}
      ]}}
    }"#;

    #[test]
    fn parses_publish_js_arrays() {
        assert_eq!(js_array(SAMPLE, "var urls"), vec!["xxbgwypx.cc/"]);
        assert_eq!(js_array(SAMPLE, "var subdomains"), vec!["thu", "pku"]);
        assert_eq!(
            js_array(SAMPLE, "var foreverUrls"),
            vec![
                "https://huangguoai.com",
                "JumpPage",
                "https://huangguoai.ai"
            ]
        );
    }

    #[test]
    fn poster_list_keeps_public_https_origins_in_sort_order() {
        let value: Value = serde_json::from_str(POSTER).unwrap();
        let items = poster_items(&value);
        let urls: Vec<&str> = items.iter().filter_map(|i| i["url"].as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://ec2b.rlhjeyuvw.cc",
                "https://huangguoai.pages.dev",
                "https://huangguoai.com"
            ]
        );
        assert_eq!(items[0]["name"], "最新地址");
        assert_eq!(items[0]["host"], "ec2b.rlhjeyuvw.cc");
        assert_eq!(items[1]["note"], "大陆可访问");
        assert_eq!(items[2]["note"], "需梯子");
    }

    #[test]
    fn poster_without_usable_entries_is_empty() {
        for raw in [
            r#"{"status":0}"#,
            r#"{"data":{"config":{"addresses":[]}}}"#,
            r#"{"data":{"config":{"addresses":[{"url":"https://huangguoai.com/x","sort":1}]}}}"#,
        ] {
            assert!(
                poster_items(&serde_json::from_str(raw).unwrap()).is_empty(),
                "{raw}"
            );
        }
    }

    #[test]
    fn probe_rejects_pages_without_business_config() {
        assert!(check_poster_body(POSTER).is_ok());
        assert!(check_poster_body(r#"{"data":{"config":{"addresses":[]}}}"#).is_ok());
        assert!(check_poster_body(r#"{"status":0}"#).is_err());
        assert!(check_poster_body(
            "<html><head><title>404 Not Found</title></head><body>nginx</body></html>"
        )
        .is_err());
    }

    #[test]
    fn custom_lines_only_keep_https_origins() {
        let list = parse_custom(
            r#"{"items":["https://a.example","http://b.example","https://c.example/path","https://a.example/"]}"#,
        )
        .unwrap();
        assert_eq!(list, vec!["https://a.example"]);
        assert!(parse_custom("{bad json").is_none());
    }

    #[test]
    fn resolves_relative_script() {
        assert_eq!(
            absolute("https://huangguoai.pages.dev/", "publish.js?20261103-2"),
            "https://huangguoai.pages.dev/publish.js?20261103-2"
        );
        assert_eq!(
            absolute("https://huangguoai.pages.dev/", "/a/b.js"),
            "https://huangguoai.pages.dev/a/b.js"
        );
    }

    #[test]
    fn picks_script_src() {
        let html = r#"<link rel="stylesheet" href="publish.css?1" /><script type="text/javascript" src="publish.js?20261103-2"></script>"#;
        assert_eq!(script_src(html).as_deref(), Some("publish.js?20261103-2"));
    }

    /// 手动跑：`cargo test --lib -- --ignored lines::tests::mirrors_live -- --nocapture`
    #[tokio::test]
    #[ignore = "联网测试：请求上游镜像列表，手动用 --ignored 运行"]
    async fn mirrors_live() {
        match mirrors().await {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).expect("json")),
            Err(e) => panic!("mirrors failed: {e}"),
        }
    }

    /// 手动跑：`cargo test --lib -- --ignored lines::tests::probe_live -- --nocapture`
    #[tokio::test]
    #[ignore = "联网测试：探测上游线路连通性，手动用 --ignored 运行"]
    async fn probe_live() {
        let urls = vec![
            "https://huangguoai.com".to_string(),
            "https://huangguoai.ai".to_string(),
            "https://ec2b.rlhjeyuvw.cc".to_string(),
        ];
        println!(
            "{}",
            serde_json::to_string_pretty(&probe(urls).await).expect("json")
        );
    }
}
