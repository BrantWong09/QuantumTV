# 调研：把 mpv 深度集成进 QuantumTV 播放器

日期：2026-09-08。仅调研，未写代码。

## 背景

当前播放器为前端 Plyr + hls.js（WebView2 内）。WebView2 缺 HEVC 解码扩展时网盘
HEVC-MKV 黑屏有声，现兜底方案是拉起外部 mpv.exe 独立窗口（`src-tauri/src/commands/mpv_player.rs`）。
本文调研"能否把 mpv 完全集成进来"，替代/强化现有方案。

## 结论（TL;DR）

1. **mpv 全格式解码能力 = libmpv/mpv.exe 二者完全一致**（同一个核心），"外挂 mpv 能放、
   内嵌播放器放不了"的差距本质是 WebView2 解码器的差距。mpv 能完整覆盖 WebView2
   缺失的 HEVC/AC-3/DTS 等，但它的 HLS 去广告能力**弱于**现有 hls.js 自定义
   loader 路径（详见 §6）。
2. **三条集成路线**：libmpv 进程内渲染（重）、mpv.exe --wid 嵌入 HWND（中）、
   mpv.exe + JSON IPC 外部受控窗口（轻）。Windows 上 render API 官方仅支持
   OpenGL 与 software 两种后端（**无原生 D3D11**），libmpv 路线在 Tauri/WebView2
   场景下工程成本最高；推荐先走路线 C（IPC 深度受控 + 单实例守护 + 可选 --wid），
   该路线能解决现有实现"无法感知播放状态/无法真正单实例/进度只进不出"的全部痛点。
3. **现有 `launch_mpv` 的一个关键事实错误**：mpv **没有**内置单实例机制。手册中
   不存在"重复启动把文件交给已有实例"的行为；当前代码注释"mpv 收到新 URL 会复用
   窗口换源"在实际第二次 `spawn` 时会开出**第二个进程/窗口**（仅当子进程意外
   存活时复用判定成立，但换源仍是新进程新窗口）。正确做法是自己管：`--input-ipc-server`
   命名管道 + `loadfile` 换源。
4. 许可：mpv 核心默认 GPLv2+；libmpv-2.dll 以 GPL 二进制形式分发到
   CC BY-NC-SA 4.0 的 QuantumTV 内不产生"传染"问题（GPL 只约束 mpv 源码派生物），
   但**分发 dll 时必须随附 mpv 的 GPL 源码获取途径声明**；若自行以 LGPL 方式构建
   （`-Dgpl=false`）可弱化，但 shinchiro 预编译包均为 GPL。

---

## 1. libmpv 进程内嵌入

### API 定位与稳定性

- libmpv = 把 mpv 作为库用 `mpv_create()` 嵌入其他应用，client API 文档直接写在
  `include/mpv/client.h`。当前 client API 版本 **2.5**（master，2026-09 查证）。
- API 版本化，变更记录在 `DOCS/client-api-changes.rst`；官方原文："The C API itself
  will probably remain compatible for a long time, but the functionality exposed by
  it could change more rapidly."（选项/属性可能改名或变取值范围，需防御式编程）。
  https://github.com/mpv-player/mpv/blob/master/include/mpv/client.h:207-222
- 许可分层：client API 为 ISC，但 "the mpv core is by default still GPLv2+ — unless
  built with -Dgpl=false, which makes it LGPLv2+"
  （https://github.com/mpv-player/mpv/blob/master/include/mpv/client.h:17-20）。

### Rust 封装现状

| crate | 版本 | 更新 | 说明 |
|---|---|---|---|
| `libmpv2` | 6.0.0 | 2026-05-12 | ParadoxSpiral/libmpv-rs 的社区续命 fork（kohsine/libmpv-rs），LGPL-2.1，要求 libmpv client API 2.0（mpv ≥ 0.35），有 anlumo 贡献的 render 实现（OpenGL/SDL2 示例），总下载 7.4w |
| `libmpv-rs` | crates.io 404 | — | 原 ParadoxSpiral 仓库已不在 crates.io 发布 |
| `libmpv-sirno` | 2.0.2-fork.1 | 2022-12 | 停更 |

来源：https://crates.io/api/v1/crates/libmpv2 （2026-09-08 查询）；
https://github.com/kohsine/libmpv-rs 。
`libmpv2` 处于"有人接手但单人维护"状态，绑定 API 2.0+，对 master（API 2.5）新属性
无绑定，需要自己透过 `mpv_command`/`set_property` 裸字符串通道补（可行但繁琐）。

### render API 与渲染后端（Windows 关键约束）

`include/mpv/render.h` 官方原文（master，2026-09 查证）：

> Supported backends
> ------------------
> OpenGL: via MPV_RENDER_API_TYPE_OPENGL, see render_gl.h header.
> Software: via MPV_RENDER_API_TYPE_SW, see section "Software renderer"

**没有 D3D11/D3D12 后端**。即 Windows 上进程内渲染要么自建 OpenGL(WGL/ANGLE)
上下文走 `MPV_RENDER_API_TYPE_OPENGL`，要么用慢速的 `MPV_RENDER_API_TYPE_SW`
（每帧 CPU 拷贝 RGBA）。官方同时明确推荐 render API 而非窗口嵌入：

> Preferably rendering should be done in a separate thread... In general, using the
> render API is recommended, because window embedding can cause various issues,
> especially with GUI toolkits and certain platforms.
（https://github.com/mpv-player/mpv/blob/master/include/mpv/render.h:42-56）

线程模型：render 函数任意线程可调但互斥；render 线程不得反向等待其他 libmpv 调用
（否则死锁，超时后画面卡顿）；wakeup 回调内不得调用 mpv_render_*。
（render.h Threading 一节，同上）

### 官方示例覆盖

mpv-examples/libmpv 下有 cocoa / qt / qt_opengl / qml / sdl / csharp / wxwidgets /
java / simple / streamcb 等，**没有 win32/D3D11 示例**；csharp 示例是 Windows 上
P/Invoke + wid 原生窗口嵌入（不是 render API）。
（https://github.com/mpv-player/mpv-examples/tree/master/libmpv ，2026-09 查证）

### 预编译库获取与体积

- shinchiro 构建源（sourceforge `mpv-player-windows/libmpv`）：最新
  `mpv-dev-x86_64-20260830-git-e8673660ab.7z` **31.4 MB**（v3 微架构版 32.6 MB），
  内含 `libmpv-2.dll` + include 头文件。
  https://sourceforge.net/projects/mpv-player-windows/files/libmpv/ （2026-09-08 查证）
- mpv 官方 GitHub CI（git-release tag）目前只出 mpv.exe 完整播放器包
  （x86_64 msvc zip 实测 Content-Length ≈ 28.3 MB），**不再附带 mpv-dev/libmpv 包**。
  https://github.com/mpv-player/mpv/releases/tag/git-release
- 分发方式：dll 独立于应用安装包，下载放置 `%APPDATA%\com.geon.quantumtv\mpv\`
  （复用现有 locate_mpv 目录约定），安装包体积不受影响。

---

## 2. mpv.exe --wid 窗口嵌入（HWND 复用）

手册原文（`--wid=<ID|-1>`，DOCS/man/options.rst:3824-3861）：

> On win32, the ID is interpreted as ``HWND``. Pass it as value cast to
> ``uint32_t`` ... mpv will create its own window and set the wid window as parent,
> like with X11. The window will always be resized to cover the parent window fully.

要点：

- mpv 在宿主 HWND 内**自建子窗口**铺满父窗口并自动 letterbox；`-1` = 脱离回独立窗口。
- 官方对嵌入模式的定位："It's much easier to use than the render API, but also has
  various problems"（client.h:198-201）；mpv-examples README 列举的坑：强平台相关、
  X11 焦点抢占（`--input-vo-keyboard` 专为嵌入场景设的开关，options.rst:4686-4696）、
  macOS Qt 不稳；**render API 才能在视频上叠自己的 OSD/UI**。
- 与 WebView2 组合的结构性矛盾：wry 的 WebView2 在 Windows 上是**独立 HWND 子窗口**
  盖在 tao 窗口客户区上。--wid 的 mpv 子窗口若与 WebView2 平级，z-order 只能二选一
  （mpv 盖住网页 = 网页 UI 不可见；网页盖住 mpv = 视频不可见）。要"网页 UI 浮在
  原生视频上"必须：① Tauri 窗口 `transparent: true` + WebView2 透明背景，mpv 子窗口
  垫底；或 ② mpv 窗口放 webview 之下、交互区做成鼠标穿透。此路径 wry/tauri 侧无
  官方支持文档，属于未开垦区（未找到任何 Tauri+mpv --wid 的现成项目或 issue 讨论，
  GitHub 搜索仅见零星 libmpv 尝试，均未成熟 —— 无一手来源，标注为待验证）。
- 结论：--wid 技术上可行（mpv 侧文档充分），但 Tauri/WebView2 侧的合成与输入路由
  全部要自己趟，风险集中且不可控。

---

## 3. mpv JSON IPC 受控（外部进程深度集成）

### 通道

手册原文（options.rst:4591-4602）：

> On Windows, named pipes are used, so the path refers to the pipe namespace
> (``\\.\pipe\<name>``). If the ``\\.\pipe\`` prefix is missing, mpv will add it
> automatically ...

Rust 侧用 tokio `tokio::net::windows::named_pipe::ClientOptions::open()` 连接即可，
无需额外依赖。协议为逐行 JSON（`{"command": [...]}` → `{"error":"success","data":...}`），
官方警告协议无鉴权加密，仅限本机使用（DOCS/man/ipc.rst:10-15）。

### 能力面（IPC 能做什么）

- 命令：全部 input 命令 + 协议专属命令（get_property / set_property /
  observe_property / observe_property_string / unobserve_property / request_log_messages /
  enable_event / get_time_us / client_name / get_version），支持 async、named args、
  request_id（DOCS/man/ipc.rst）。
- `loadfile <url> [replace]` 换源不换窗口；`seek`、`stop`、`quit` 等。
- 属性观察：`observe_property` 持续推送 property-change 事件，**必须保持连接不关**
  （ipc.rst:267-295 原文 "You must keep the IPC connection open to make it work."）。
- 关键属性（读写性经手册属性章节惯例与 IPC 示例交叉确认；playback-time/pause/volume
  出现在官方示例中，duration/eof-reached/track-list 为只读属性）：
  playback-time(RW，写=seek)、time-pos(RW)、percent-pos(RW)、pause(RW)、speed(RW)、
  volume(RW)、mute(RW)、duration(RO)、eof-reached(RO)、track-list(RO)、media-title(RO)。
- 意义：接入 IPC 后，外部 mpv 就能拿到与 Plyr 相同的控制面——进度上报
  （playback-time 观察 → save_play_progress）、跳片头片尾（收到 tick 决策后 seek）、
  播放结束自动下一集（eof-reached 观察）、倍速/音量同步、前后台 UI 显示真实状态。

### 单实例（现有实现的错误假设）

- 手册（options.rst 全文 + ipc.rst）**不存在**任何"mpv 重复启动时把文件交给已有
  实例"的机制；无 `--single-instance` 类选项。每次 `std::process::Command::spawn`
  都是全新进程新窗口。
- 现有 `mpv_player.rs:78` 注释 "同一子进程实例: mpv 收到新 URL 会复用窗口换源"
  不成立：只有当 `MPV_CHILD` 里的旧子进程恰好还活着，且……新 spawn 的进程仍是
  独立进程，不会把 URL 交给旧进程。实测表现会是"切集/换源越切窗口越多"。
- 正确实现：启动时 `--input-ipc-server=\\.\pipe\quantumtv-mpv --idle=once`，宿主
  持有管道连接；换源 = 管道发 `loadfile <url> replace` + `set_property start <sec>`；
  退出 = `quit`。`--idle` 让 mpv 无文件时不退出（options.rst:850-856：
  "Makes mpv wait idly instead of quitting when there is no file to play"）。

### 进度回传通道（现状缺口）

外部 mpv 的播放进度目前只进不出（launch 时带 `start_at` 单向注入）。IPC 通道补上
observe playback-time 后，可以把 mpv 的进度写回 save_play_progress，实现"外部
mpv 看一半 → 回 webview 播放器续播"闭环。

---

## 4. WebView2 透明合成（可选 UI 覆盖层）

- Tauri v2 窗口支持 `transparent`（tao/wry 已实现 Windows 透明 webview；WebView2
  本身支持 DEFAULT background 透明，`put_DefaultBackgroundColor(alpha=0)`）。
- 组合：tao 主窗口 → 子 1：mpv --wid HWND（视频层）；子 2：透明 WebView2（UI 层），
  需要 WebView2 控件设 `WS_EX_TRANSPARENT`/`WS_EX_NOREDIRECTIONBITMAP` 类穿透或
  按坐标转发点击，wry 没有现成 API。**未找到任何成熟开源先例**（无一手来源）。
- 判断：即便走通，鼠标事件、滚轮、键盘焦点、DPI、全屏切换都是自研成本；除非产品
  上强需求"mpv 画面 + 网页自定义控制条"合体，否则不建议在此分支投入。

---

## 5. mpv 流媒体能力 vs 现有 hls.js 路径

### mpv 内建能力（对手工直链/网盘直链是净增益）

- 解码：内置 ffmpeg 全量解码器（HEVC/AV1/AC-3/DTS…），WebView2 的解码缺口全部补齐。
- HLS(m3u8)/DASH(mpd) 由 ffmpeg/lavf demuxer 原生支持（含 AES-128 加密 HLS 的
  标准 ffmpeg 解密路径）；直播流边下边播、`--demuxer-max-bytes` 缓存调优（现
  代码已用）。
- 网络选项（options.rst:5696-5719）：
  - `--user-agent=<string>`、`--http-header-fields=<field1,field2>`（string list，
    示例即自定义请求头注入）、`--referer`、`--cookies`、`--http-proxy`。
  - master 新增 libcurl 后端（HTTP/2/3、自动解压），仍遵守上述网络选项
    （options.rst:5800-5815）。旧版走 ffmpeg 网络层，同样支持这些选项。
- 本地代理 URL：`http://127.0.0.1:<port>/netdisk/file.mp4?url=...` 是标准
  HTTP + Range，mpv/ffmpeg 直接支持，无需改动 netdisk_proxy。
- QuantumTV 的网盘场景（UA 校验、Referer、直链带签名）因此**可以**完全脱离
  netdisk_proxy 由 mpv 原生带头发请求（`--http-header-fields` + `--user-agent`），
  代理仅作 webview 播放路径需要而保留。

### 与 hls.js 自定义 loader 的差距（mpv 侧的净损失）

现有 webview 播放路径在 Rust 侧做两件事（`TauriHlsJsLoader` + `fetch_m3u8`）：

1. **去广告**：`filter_ads_from_m3_u8`（crates/core/src/playback.rs:68）在 manifest
   层面剔除 CUE-OUT/CUE-IN 广告块、广告 DATERANGE、URL 特征分片（/ad/、_ad_、
   promo、doubleclick），改写后的 m3u8 喂给 hls.js。
2. **自定义请求头/缓存/预取**：manifest 与 TS 分片都经 Rust `fetch_m3u8`/`fetch_binary`
   （带缓存统计、失败重试参数）。

mpv 侧**没有等价的 manifest 重写钩子**：

- libmpv 提供 `stream_cb` 自定义流 API（mpv-examples/libmpv/streamcb 示例），理论上
  可以在 Rust 侧接管所有网络请求并喂入改写后的数据 —— 但 mpv.exe 外部进程路线
  用不了 stream_cb（那是 libmpv 进程内 API）。
- 外部 mpv.exe 可用的只有：`--http-header-fields`/`--user-agent`（等价"带请求头"），
  以及 ytdl-hook 类脚本钩子（对 m3u8 直链无意义）。**manifest 去广告在外部 mpv
  路径上无法复用**。
- 缓解选项：a) 接受外部 mpv 播放时不去广告（HEVC 网盘源本来就没有广告分片问题，
  广告主要出现在 CMS 采集站 m3u8 —— 这类源 WebView2 解码没问题，不会走 mpv）；
  b) 若未来想让 mpv 也播 CMS m3u8，可在本地代理层做 m3u8 改写（把 netdisk_proxy
  扩展成通用 rewrite proxy，对 mpv 隐藏复杂度）—— 工作量中等，属于后续可选优化。

### 结论

"mpv 替换 hls.js 做默认播放器"**不划算**：丢掉去广告与 loader 缓存统计，换来的
只是 codec 兜底；而"WebView2 可播的源继续 hls.js、解不动的源（HEVC/网盘直链/直播）
走受控 mpv"是零损失组合。

---

## 6. 三条路线对比

| 维度 | A. libmpv 进程内 | B. mpv.exe --wid 嵌入 | C. mpv.exe + JSON IPC 受控 |
|---|---|---|---|
| 解码能力 | 全量 | 全量 | 全量 |
| UI 一体化 | 最好（需自建 GL 层） | 中（子窗口遮挡问题） | 差（独立窗口，任务栏双图标） |
| 进度/状态回传 | 内存直读 | 需另开 IPC | 原生 observe_property |
| 单实例换源 | 天然 | 进程管理自研 | loadfile replace，清晰 |
| 去广告 m3u8 | streamcb 可做，工作量大 | 不可用 | 不可用 |
| Windows 渲染后端 | 仅 OpenGL/soft，需 WGL/ANGLE | 无关（mpv 自绘） | 无关 |
| 依赖 | libmpv-2.dll（31MB 压缩包）+ libmpv2 crate（单人维护） | mpv.exe | mpv.exe（现有约定不变） |
| Tauri/WebView2 冲突 | render 层与 webview 合成自研 | HWND z-order/透明自研，无先例 | 无冲突 |
| 工作量估计 | 大（3-5 人周级，风险高） | 中（2-3 人周级，合成未知数） | 小（2-4 天级） |
| 许可 | GPL dll 随包分发需附源码声明 | 同左 | 同左（且用户自备） |

## 7. Web 原生解码播放器横向对比（2026-09-09 补充）

除了 mpv，还有没有"web 原生、解码能力强大"的播放器可以替代现有 Plyr+hls.js 栈？
按解码能力来源分两类查证。

### 7.1 前提事实：WebView2 的 HEVC 支持边界

- Chromium 自 **107**（2022-10）起原生支持 HEVC 硬解播放："Chrome 107, which
  supports HEVC hardware decoding for all platforms 'out of the box', if the
  hardware is supported"；Windows 7+ 仅限"devices with supported hardware"。
  （Wikipedia HEVC → Software support，转引 Chrome 107 发布说明：
  https://en.wikipedia.org/wiki/High_Efficiency_Video_Coding#Software_support ）
- Microsoft Edge 更早（77 起）在 Windows 10 1709+ 依赖系统 HEVC Video Extensions
  + 受支持硬件提供支持（同上来源）。
- Chromium **没有 HEVC 软解回退**：文档通篇限定 hardware decoding。
- 推论：QuantumTV 在无 HEVC 硬解的机器（老 CPU/老显卡/缺扩展）上黑屏有声，
  是**机器级缺解码器**，不是 WebView2 缺能力。任何走系统硬解通道的 Web 播放器
  （WebCodecs 系）在该机器上同样无解。

### 7.2 WebCodecs 硬解路线（与 `<video>` 同源，救不了无硬解机器）

| 项目 | 许可 | 现状 | 与 hls.js 栈的关系 |
|---|---|---|---|
| **xgplayer**（bytedance） | MIT | v3.0.26 活跃；自研 FLV/HLS/DASH 解析器（"A HTML5 video player with a parser that saves traffic"）；v2 生态有 h264/h265/aac **解析**工具（xgplayer-helper-codec），未见独立 wasm 软解 HEVC 内核 | 协议/容器层与 hls.js 重叠，无解码增益 |
| **flv-h265.js / flv-h265** | 各异 | 独立 npm 包，HEVC-FLV + MSE 方案 | 只补 FLV+HEVC 这一窄场景 |

结论：这一类播放器的解码通道就是系统硬解（WebCodecs / MSE 直通），
**HEVC 黑屏问题上一分钱增益都没有**；对 QuantumTV 的潜在价值仅在协议层
（FLV/增强 MP4/DASH），而当前源以 m3u8 为主，hls.js + 自定义 loader 已覆盖，
不值得替换。

### 7.3 WASM 软解路线（自带解码器，不挑机器）

**libmedia**（zhaohappy/libmedia，LGPL-3.0，372 star，npm `@libmedia/avplayer`
0.2.0，repo 最后 push 2026-06-27）是唯一真正"web 原生 + 全量解码"的候选：

- 架构：TS 做 demux（可脱 SharedArrayBuffer/Worker 运行），解码用从 FFmpeg
  libavcodec 编译的 **WASM**，有 WebCodecs 时切硬解。API 仿 FFmpeg。
  （https://github.com/zhaohappy/libmedia README）
- 软解覆盖：hevc、av1、vvc、vp8/9、mpeg1/2/4、wmv；音频 ac3/eac3、dts、wma、
  flac 等。硬解（WebCodecs）仅 h264/hevc/av1/vp8/9 + aac/mp3/opus/flac，其中
  hevc "只支持硬解"。
- 容器：matroska/webm、mpegts、mp4、flv、ogg…（输入）；协议：hls、dash、rtmp、
  rtsp（仅输入）。
- 体积策略：每个编解码器单独编译 wasm，按需加载；分 baseline/atomic/simd/64
  四档，"simd … 性能最高"，但"目前只有 h264 的 simd 解码器是手动优化的"。
- 许可风险：本体 LGPL-3.0，"某些依赖库是 GPL 协议，如果你使用了这些依赖库则
  libmedia 将被传染为 GPL 协议"（dist/encoder 下 x264/x265）。
- 未列任何生产案例（README 无采用方信息，标注为风险项）。

**对 QuantumTV 的适配代价**：
- 4K HEVC wasm 软解性能存疑：simd 手动优化只做了 h264，HEVC 走编译器自动
  向量化；多线程软解依赖 SharedArrayBuffer（COOP/COEP 响应头），Tauri 静态
  服务需自行加头。目标机器恰是无硬解的老机器 → CPU 也弱，软解 4K 大概率卡。
- QuantumTV 的去广告 manifest 重写、TauriHlsJsLoader 缓存/预取/测速全部要
  在 libmedia 的 IO 层重新实现一遍。
- 播放器壳（Plyr UI、手势层、快捷键、跳片头片尾 tick 集成）全部重接。

### 7.4 横向结论

| 方案 | HEVC 无硬解机器 | 去广告 m3u8 | 改造成本 | 定位 |
|---|---|---|---|---|
| 现有 Plyr+hls.js | ✗（黑屏兜 mpv） | ✓ | 0 | 默认播放器 |
| WebCodecs 系（xgplayer 等） | ✗（同源硬解） | 需重写 | 中 | 无增益，不采纳 |
| libmedia（wasm 软解） | ✓ 理论可行，性能存疑 | 需重写 | 大 | 1080p 级备选，先 demo 验证帧率再评估 |
| mpv 兜底（路线 C） | ✓ 原生硬解 | ✗（可代理层补） | 小 | 解码兜底正解 |

维持 §7 之前的推荐不变：hls.js 栈 + 受控 mpv 兜底。libmedia 若要跟进，先做一个
独立 demo 页在目标低配机器上实测 1080p/4K HEVC 软解帧率，数据说话。

## 8. 对 QuantumTV 的落地建议

> **落地状态（2026-09-09）**：已按 **方案 B + IPC 受控（B+C 组合）** 实现，
> commit `307eed5`（分支 `feat/player-optimize`）：
> - Rust `commands/mpv_embed.rs`：主窗口内 STATIC 黑底子窗口作 mpv 渲染宿主
>   （`--wid`），JSON IPC 命名管道（`\\.\pipe\quantumtv-mpv-embed`）下发
>   loadfile/seek/pause/speed/volume；`observe_property` 回传
>   playback-time（200ms 节流）/duration/pause/eof-reached → Tauri 事件
>   `mpv-embed-event`；退出链路 quit → kill → 宿主窗口隐藏，进程退出兜底。
> - 前端 `play/page.tsx`：mpv 模式下视频 rect（扣除底部 48px DOM 控制条）
>   经 `mpv_embed_sync` 按 DPR 同步；控制条含播放/上下集/进度/倍速/音量/
>   切回内置；快捷键经 IPC 转发；`player_tick` 复用实现进度保存与
>   跳片头片尾；eof 自动连播；切到 m3u8 源自动回退内置播放器。
> - 非 Windows 平台命令返回明确错误，行为不变。旧 `launch_mpv`（独立
>   窗口）保留未删，前端已不再调用。
> - 待真机验证项：WebView2 与子窗口的 z-order 表现（窗口缩放/最小化恢复/
>   页面全屏切换时的遮挡关系）、mpv 管道就绪时序、老机器 HEVC 软解帧率。

**推荐路线 C：受控外部 mpv（IPC 深度集成），分三步**：

1. **修正确性（必做）**：`mpv_player.rs` 重写为管道受控模型——
   启动 `--input-ipc-server=\\.\pipe\quantumtv-mpv --idle=once --force-window=immediate`，
   宿主用 tokio named pipe 常驻连接；换源/切集 = `loadfile ... replace`；
   复用逻辑由宿主 100% 掌控（进程死了重拉，活着就 loadfile）。删除错误的
   "再 spawn 一次即复用"假设。
2. **状态闭环（高价值）**：observe `playback-time`/`eof-reached`/`pause` →
   推送回前端（Tauri event）；接入 save_play_progress、自动下一集、跳片头片尾
   seek；前端播放页为 mpv 模式渲染"镜像控制条"（进度/暂停/倍速/音量），操作经
   IPC 下发。用户体验从"弹出去另一个窗口"变成"界面内可控的子播放器"。
3. **可选 --wid 模式（低优先）**：在设置里提供"嵌入播放页"实验开关：把主窗口
   客户区一个预留 HWND 交给 mpv --wid，webview 半透明覆盖。需要专门处理
   z-order/鼠标穿透，且无社区先例，按验证性项目对待，不阻塞 1、2。

**不推荐**本分支直接上 libmpv 进程内渲染（路线 A）：Windows 无 D3D11 render
后端、Rust 封装单人维护、需要自建 GL(WGL/ANGLE) 渲染栈与死锁规避，投入产出
比明显劣于路线 C；若未来 WebView2 生态变化（如 HEVC 扩展普及率提升）或
libmpv2 crate 成熟出 D3D11 支持再评估。

## 附：关键来源清单

- mpv 手册 options（--wid / --input-ipc-server / --user-agent / --http-header-fields /
  --idle / --input-vo-keyboard）：https://github.com/mpv-player/mpv/blob/master/DOCS/man/options.rst
- JSON IPC 协议：https://github.com/mpv-player/mpv/blob/master/DOCS/man/ipc.rst
- client.h（嵌入方式对比 / API 版本 2.5 / 许可分层）：https://github.com/mpv-player/mpv/blob/master/include/mpv/client.h
- render.h（后端仅 OpenGL/Software、线程模型）：https://github.com/mpv-player/mpv/blob/master/include/mpv/render.h
- client-api-changes.rst（API 版本史）：https://github.com/mpv-player/mpv/blob/master/DOCS/client-api-changes.rst
- mpv-examples（嵌入方式与示例矩阵、无 win32 render 示例）：https://github.com/mpv-player/mpv-examples
- libmpv2 crate：https://crates.io/crates/libmpv2 / https://github.com/kohsine/libmpv-rs
- shinchiro 预编译（mpv-dev 31.4MB 7z）：https://sourceforge.net/projects/mpv-player-windows/files/libmpv/
- mpv 官方 CI 构建（仅 mpv.exe，无 mpv-dev）：https://github.com/mpv-player/mpv/releases/tag/git-release
- Chromium HEVC 硬解起始版本与条件：https://en.wikipedia.org/wiki/High_Efficiency_Video_Coding#Software_support （转引 Chrome 107 发布说明，2026-09-09 查证）
- xgplayer：https://github.com/bytedance/xgplayer （README 自研解析器描述、MIT 许可，2026-09-09 查证）
- libmedia：https://github.com/zhaohappy/libmedia （README 软解覆盖/体积策略/许可声明，2026-09-09 查证）；npm https://registry.npmjs.org/@libmedia/avplayer

（除注明"无一手来源"外，以上结论均有一手文档/源码支撑；查证时间 2026-09-08，
§7 补充部分查证于 2026-09-09。）
