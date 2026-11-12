# 客户端内容与网络安全

## 本次加固的行为

- 文章正文通过固定版本 DOMPurify 清洗，并且将 `DOMPurify.sanitize` 作为进入 `dangerouslySetInnerHTML` 前的最终步骤（幂等处理代理图片，避免二次解析差异）。严格模式下只保留必要的 HTML 排版、链接和图片，SVG、MathML、脚本、事件属性、内联样式、非 HTTP(S) URL 及本机/内网主机名均不进入页面；正文容器统一拦截 `<a>` 点击并交由外部打开或复制，防止 WebView 原地导航或弹出无守卫新窗口。
- 发布版启用 CSP：不允许内联/远端/data URL 脚本，不允许 iframe、表单与页面基址覆盖。Blob 仅用于媒体和 hls.js Worker，不在 `script-src` 中放行。应用主窗口拒绝跳转到远端页面（开发模式下亦限制 `1420` 端口仅允许本机或显式配置的 `TAURI_DEV_HOST`），媒体协议仅向应用来源返回 CORS 授权。
- 图片、HLS、分片、线路探测、镜像清单（站点地址接口 `/api/site/address-poster` 与地址发布页）及更新元数据的请求经过统一目标检查。只允许 HTTP(S)，拒绝 URL 中的用户名/密码；按主机和端口缓存已校验并固定 DNS 地址的客户端 30 秒（配置代理时改由代理远端解析，见「代理存在时的 DNS 处理」）；到期重新检查全部 DNS 结果，混合公私网答案整体拒绝。GET 最多跟随 5 次重定向，每一跳重新检查，禁止 HTTPS 降级到 HTTP。HLS 的子清单、备用音轨、密钥及低延迟分片 URI 也经媒体代理改写，不能绕过目标检查。禁用可从外部 JSON 派生新地址的可选插播/内容引导功能。
- 业务线路必须是纯 HTTPS origin，不接受路径、查询串、片段或认证信息；远端下发的清单条目在进入界面前也按同一规则过滤，隐藏项、带路径或指向内网的地址一律丢弃。密码登录和注册仅允许 `https://huangguoai.com`、`https://huangguoai.ai` 两个固定站点；远端镜像列表不能扩大账号信任范围。镜像保留游客浏览。
- token 与具体站点 origin 绑定，跨站点切换清除登录状态。在途请求捕获自己的站点快照，不能把旧 token 带到新站点。包含账号信息的业务 POST 不跟随任何重定向，避免 307/308 重发密码。旧版会话没有站点归属，升级后需要重新登录。
- 网络诊断只返回代理协议、主机和端口；隐藏代理用户名、密码、路径、查询串、片段及 NO_PROXY 列表。无效代理配置隐藏整个原始值。脱敏发生在 Rust 返回数据之前，因此后续写入日志和复制剪贴板均得到脱敏结果。此前已生成的日志/已复制的诊断内容不会自动重写。

## 严格安全模式与兼容回退（命令行 / 设置开关）

默认开启**严格安全模式**。若上游正文排版、特殊内网环境或自定义 FakeIP 网段因严格规则导致功能受阻，可通过以下任一方式回退到兼容模式（关闭激进标签/内网/导航守卫，仍保留基础 DOMPurify 脚本与事件清洗及应用内部协议隔离）：

1. **命令行参数**：启动时传入 `--no-strict`（或 `--no-strict-security`、`--relaxed`），也可设置环境变量 `HGHUB_STRICT=0`；传入 `--strict` 可显式强制开启。
2. **设置页开关**：在应用「设置 → 网络 → 严格安全模式」中切换，立即生效并持久化到应用数据目录 `security.json`。

## FakeIP 与内网策略

明确放行 `198.18.0.0/15`，即 `198.18.0.0` 至 `198.19.255.255`，兼容常见代理软件的 FakeIP。此例外同样适用于 DNS 结果、重定向及 IPv4 映射 IPv6 地址。

拒绝普通私网、回环、链路本地、共享地址空间、组播和保留地址，包括 `10/8`、`172.16/12`、`192.168/16`、`127/8`、`169.254/16`、`100.64/10`、IPv6 ULA/链路本地/回环，以及可封装私网 IPv4 的 NAT64、6to4、Teredo 地址。拒绝 localhost/.localhost、.local、.internal、home.arpa 主机名。其他自定义 FakeIP 网段没有自动放行。

已知兼容限制（P2-3）：严格模式拒绝 NAT64/DNS64 合成地址（包括 `64:ff9b::/96`），且混合公私网答案整体拒绝。因此 IPv6-only 运营商网络若依赖 DNS64，直连媒体或 API 可能无法使用。可使用同时提供正常公网 DNS 解析的可信代理环境，或在了解风险后通过设置关闭严格模式 / 使用 `--no-strict` 回退；这不会自动放行其他自定义 FakeIP 网段。

系统代理和 `HGHUB_PROXY` 是用户配置的可信传输层，仍可连接 `127.0.0.1:7897` 等本地代理端点；这不意味着媒体 URL 可以指向该地址。直接连接会使用检查后固定的 DNS 结果；HTTP(S) 代理可能自行解析域名，FakeIP/TUN 也会重新映射目标，应用无法独立验证代理/TUN 最终实际连接的地址。需要同时信任并配置好代理软件；本策略不替代代理侧内网访问控制。

## 代理存在时的 DNS 处理

- 检测到 `HGHUB_PROXY` 或 `HTTP(S)_PROXY` / `ALL_PROXY` 时，不再在本地解析并固定目标地址：域名交给代理远端解析，避免把连接固定到污染结果（例如永久域名被解析到 Facebook 网段）。协议、主机名、IP 字面量、私网与回环的检查仍在连接前执行。
- 没有代理时保留解析与固定；若全部答案都落在已知污染网段，则跳过固定、交由连接时重新解析，而不是直接报错。该判定只影响「是否固定地址」，不放宽 `allowed_ip`，也不改变混合公私网答案整体拒绝的规则。
- 代理仍必须是固定 HTTP 代理：PAC（`AutoConfigURL`）不生效，Android 需要 TUN 模式；SOCKS5 没有编入当前构建，请填客户端同时提供的 HTTP 端口。
- 「线路 → 检测」现在请求 `/api/site/address-poster`，并要求返回 JSON 且带 `data.config`。首页 200 不再视为可用：跳转页与静态发布页首页同样是 200，但 `/api/**` 恒为 404。
- 自定义线路保存在应用数据目录 `custom_lines.json`，只接受纯 HTTPS origin（无路径、查询串与认证信息），上限 12 条。

## 下载功能的写盘约束

「设置 → 下载」与播放页的下载按钮新增了 9 个应用自有命令（`download_settings`、`download_dir_set`、`download_dir_reset`、`download_open_dir`、`dir_roots`、`dir_list`、`dir_create`、`download_start`、`download_status`、`download_cancel`）。它们不引入 `tauri-plugin-fs` 或 `tauri-plugin-dialog`，前端没有获得 `fs:*` / `dialog:*` capability，写盘只发生在 Rust 侧：

- 目录参数必须是绝对路径、不含控制字符且长度受限；选定目录时先 `create_dir_all` 再写一次 `.hghub-write-test` 探针，只有探针成功才保存设置，失败直接返回可读原因（权限不足、目录被移除、只读盘）。开始下载前会在「剧集目录」上再探一次，避免设置之后目录被用户删改。
- 目录浏览只返回子目录名，不返回文件名或内容；`.` 开头的隐藏目录不列出，单次最多 300 项。目标目录不存在时回退到最近的已存在祖先，不会在浏览阶段创建目录。
- `download_open_dir` 只在 Rust 侧调用 `opener.open_path` 打开目录，仍然不向前端授予 `opener:*` capability；路径来自应用自己的设置或本次目录浏览结果，不接受前端传入任意 URL。
- 媒体地址、子清单、密钥与分片仍走 `net::fetch_bytes`，沿用统一的目标检查、重定向限制与严格模式，不因为下载而放宽内网/明文限制。
- 移动端受系统沙箱限制不能自选外部目录，固定写入应用私有目录，不申请存储权限；`Settings::editable` 为 `false`，前端不显示「更改」入口。
- 分片按 `EXT-X-KEY: METHOD=AES-128` 在内存中解密后再顺序写入，先写 `.hghub-part` 临时文件，成功后改名，失败或取消时删除残留，避免留下半截文件；单集任务进行中不会并发启动第二个任务。

## 本地回归验证

```sh
pnpm build
cargo check --locked --manifest-path src-tauri/Cargo.toml
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib
```

以上 Rust 测试默认跳过 5 项 `#[ignore]` 联网测试（`media::tests::live_play_chain`、`media::tests::cover_decrypt_works`、`lines::tests::mirrors_live`、`lines::tests::probe_live`、`net::tests::diag`），结果为 28 passed / 0 failed / 5 ignored，覆盖普通内网/FakeIP 边界、IPv4 变体、IPv6、混合 DNS 答案、重定向、凭据站点绑定、旧会话和代理脱敏，不发送真实账号信息。

需要真联网时显式跑：`cargo test --locked --lib -- --ignored`。其中封面测试要求当次有效的地址，用 `HGHUB_TEST_COVER_URL` 传入；播放链路与线路探测依赖上游实时接口。回归测试不再硬编码带 `auth_key` 时效签名的 URL——旧写法的签名已于 2026-11-03 过期，会让 `cargo test --lib` 稳定失败。

浏览器回归入口为 `tests/html-security.html`。启动 `pnpm dev --host 127.0.0.1` 后，在浏览器打开 `http://127.0.0.1:1420/tests/html-security.html`；结果应为 `passed: 31, failed: 0`。测试使用无害页面变量，覆盖严格及兼容模式的原 SVG 漏洞、混淆 URL、事件属性、命名空间/HTML 变异样本，以及正常排版、懒加载图片、重复清洗和安全链接。

本次验证包括 macOS 上的前端构建、Rust 编译和本地测试，以及独立 Chromium 的 15 项正文回归与 5 项生产 CSP 检查；没有启动真实 Tauri 应用连接上游，也没有完成 Windows/Android 实机播放和真实 FakeIP/TUN 联调。

2026-11-07 本地复测：Linux 上前端生产构建、回归测试 TypeScript 检查及 `cargo check --locked` 通过；Rust 本地测试 16 项通过、0 失败（跳过上述 6 项联网测试）。独立 Chromium 分别模拟 Windows、macOS 用户代理，验证 `http://hg.localhost` 与 `hg://localhost` 两套图片协议分支，每套 29 项全部通过；这不等同于原生 Windows/macOS WebView 实测。复测修复了最终 URL 规则误删链接 `target`/`rel` 和 macOS 代理图片重复清洗丢失的问题。Linux 编译库仅下载、解包到 `/tmp` 使用，未安装到系统。

2026-11-07 原生补测：系统 GTK/WebKit 依赖安装完成后，构建嵌入生产前端资源的 Tauri 2 调试程序，在 Linux WebKitGTK 2.52.6 中通过 WebDriver 验证。使用独立数据目录，12 次启动均完整关闭原生进程后再启动，116 项检查通过，覆盖默认启动静音、手动开声后连续重启、关闭/重新开启偏好、音量记忆、操作后立即关闭，以及普通视频和文章视频共享静音状态。媒体使用本地 WAV 替身，业务内容接口使用测试数据；设置状态、WebKit 磁盘存储和窗口关闭 IPC 均来自真实客户端。已有正文安全回归在原生 WebView 中 29 项全部通过；这仍不等同于 Windows/macOS/Android 实机或真实上游播放链路验证。

同次重跑全部 Rust 测试，未跳过任何项目：19 项通过、3 项失败。`mirrors_live` 与 `live_play_chain` 因当前 DNS 将目标解析为普通私网地址而被严格策略拒绝；`playlist_rewrite_works` 使用的旧地址返回 HTTP 403。此前跳过的封面解密、线路探测和网络诊断测试完成运行，其中探测/诊断测试通过仅代表返回结果可生成，不代表上游可达。未因此放宽内网访问限制。

审查后续修正与未采用建议的逐项说明、对应 commit 及验证范围，见 [安全审查后续复核](review-followup.md)。本轮前端生产构建（含测试类型检查）、Rust 编译检查通过；本地 Rust 回归 28 项通过（5 项联网测试标记 `#[ignore]`），浏览器回归 70 项通过。安全模式现在仅由 Rust 的 `security.json` 持久化，前端收到后端确认后才切换；诊断 DNS 仅报告数量和 FakeIP 标记。匿名业务 GET 可逐跳校验后跟随 301/302/303，带凭据/请求体的业务请求不跟随；只有无请求体的逻辑 GET 允许重试。HLS 时间线元数据保留，外部 URI 和资源列表字段仍过滤。

本次仅处理审查编号 1、3、5、8；媒体/日志容量上限和 token 明文存储仍需另行处理。外链改用固定版本 `tauri-plugin-opener` 2.7.2（锁定 `open` 5.4.5），HTTP(S) URL 仍经 Rust 地址策略校验，以默认处理器 `with = None` 打开。Windows 路径启用 `shellexecute-on-windows`，通过 `ShellExecuteExW` 传递完整 URL，不经过命令解释器；关闭插件自动接管链接，且不给前端授予 `opener:*` capability，继续统一经过现有校验入口（P1-1）。插件要求 Rust 1.90，项目最低版本声明同步调整。网络错误文本的 UTF-8 截断在改动网络入口时一并改为安全截断。

参考：[DOMPurify](https://github.com/cure53/DOMPurify)、[Tauri CSP](https://v2.tauri.app/security/csp/)、[reqwest DNS 地址固定接口](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.resolve_to_addrs)。
