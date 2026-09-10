# Phase 0 现状基线分析

> 按 `12-agent-prompts.md` Prompt 1 执行：只分析，不修改代码。
> 分析基于 `feat/v2-refactor` 分支 (d11eba1) 的实际代码，核对对象为 `docs/architecture/00-current-state.md`。

## 1. 代码入口与调用链

### 1.1 播放主链路（spider 网盘源，当前主力路径）

```text
播放页挂载 (src/app/play/page.tsx)
  |
  | invoke initialize_player_view / initialize_player_by_query   ← 聚合初始化
  v
src-tauri/src/commands/video.rs
  ├─ detail: bridge → spider detailContent (crates/core/src/spider/mod.rs)
  ├─ enrich_first_episode_direct(): 首集经 playerContent 直链化
  |    └─ crates/core/src/spider/player.rs::resolve_spider_episode
  |         └─ bridge POST /playerContent → RawPlayResult (url + header)
  |    └─ unwrap_local_proxy_url(): 解开 kaiser 127.0.0.1:8096 包装
  |    └─ netdisk_proxy::ensure_started + wrap_proxy_url: 包装成本地代理地址
  ├─ 播放记录 / 收藏 / 跳过配置 / 播放器配置 (db + storage)
  v
前端 updateVideoUrl()
  ├─ isDirectPlayableUrl(): 按扩展名判定 mp4/m3u8/flv/mpd → 直接播
  └─ 非 spider 或直链 → invoke resolve_spider_episode → 同上解析链
  v
播放路径分流（前端判定）
  ├─ m3u8 → hls.js 自定义 loader → invoke fetch_m3u8 (Rust 拉取+去广告) → Plyr/<video>
  ├─ mp4 直链 → <video src> + Plyr
  |    └─ HEVC 黑屏检测 (videoWidth===0) → 兜底 launchMpvExternal()
  └─ 网盘 /netdisk/file.mp4?url=.. 直链失败 → 重新解析换链（限 1 次）
  v
mpv 路径 (方案 C)
  ├─ invoke mpv_embed_launch → Rust spawn mpv --input-ipc-server=\\.\pipe\quantumtv-mpv-embed
  ├─ invoke mpv_embed_command (loadfile / cycle pause / seek / set_property …)
  └─ Rust observe_property → emit mpv-embed-event (time 500ms 节流 / duration / pause / eof / dead)
```

### 1.2 播放进度与跳过片头片尾

```text
前端定时器 (Plyr 模式 5s 节流决策 / mpv 模式 1s interval)
  |  invoke player_tick (把 currentTime/duration/nowMs/上次保存时间 全部传给 Rust)
  v
video.rs::player_tick → decide_tick_timing + SkipDetection (crates/core/src/playback.rs)
  |                    → preload::preload_next_episode_if_needed (预载下一集)
  v
返回决策 → 前端自行 invoke save_play_progress / mpv_embed_command(seek) / 切下一集
```

### 1.3 非播放链路（不在本次重构范围，但登记备案）

- `search` / `get_video_detail(_optimized)` / `get_source_categories` / `get_home_catalog` / `get_douban_data`：内容获取，均聚合在 video.rs
- `fetch_binary` / `fetch_m3u8` / `proxy_image`：通用网络代理命令，被 hls.js loader 和图片组件使用
- `prefer_best_source_command` / `test_video_source_command`：选源（crates/core/src/source_selection.rs）
- bridge 隧道（crates/core/src/bridge/）：APK 反向隧道 + search/detail/play 透传

## 2. 文件分层归类（Source / Resolver / Media / Gateway / Playback / UI）

| 文件 | 现属层 | 目标层 | 备注 |
|---|---|---|---|
| `src/app/play/page.tsx` (3725 行) | UI **+ Resolver + Playback + 状态** | UI | 最大耦合点，见 §3 |
| `src-tauri/src/commands/video.rs` (4121 行) | Content + Resolver + Gateway 调用方 + History | 拆分到 ContentService/ResolveService/HistoryService | 兼容层保留旧 command 签名 |
| `src-tauri/src/commands/mpv_embed.rs` (314 行) | Playback backend（活路径，方案 C） | MpvBackend ← PlaybackManager | JSON IPC 封装可保留 |
| `src-tauri/src/commands/mpv_player.rs` (141 行) | Playback backend（**遗留死路径**） | 删除或并入 MpvBackend | 见 §4-⑧ |
| `src-tauri/src/commands/netdisk.rs` | 网盘账号命令 | Resolver 侧支撑 | 云端扫码登录轮询 |
| `src-tauri/src/commands/preload.rs` | History/预载 | HistoryService 边缘 | player_tick 内被调用 |
| `crates/core/src/spider/` (mod/player) | Source 执行器 + RawPlayResult 来源 | Source 层 + AndroidSpiderResolver 的原料 | player.rs 输出 (url, header) 即 RawPlayResult |
| `crates/core/src/bridge/` | Android Bridge | Source 层基础设施 | 不变，禁止接触播放器 |
| `crates/core/src/netdisk_proxy.rs` (328 行) | Gateway（雏形） | PlaybackGateway | 见 §3-⑤ |
| `crates/core/src/netdisk/mod.rs` | 网盘枚举 | Resolver 支撑 | 登录已迁 APK |
| `crates/core/src/playback.rs` (385 行) | **名不符实**：实际是 SkipDetection + m3u8 去广告 | Playback 域（PlaybackService/State/Session） | 命名冲突，见 §4-⑦ |
| `crates/core/src/types.rs` | 内容模型 (SearchResult) | Content Models | 不塞播放器状态 |
| `src/lib/types.ts` | TS 类型（手工维护） | 与 Rust 结构同步 | IPC 文档建议 |
| `src-tauri/src/commands/skip.rs` | 跳过片头片尾命令 | Playback 域边缘 | 薄封装 |

## 3. 与目标架构的冲突点

### ① 双播放器（问题 A，确认存在，且形态比文档描述的更动态）

不是简单的“两个播放器并存”，而是**运行时互切**：

- Plyr + hls.js + `<video>`（WebView 路径）是默认路径
- HEVC 黑屏检测（`page.tsx:2617` hevcFallback）自动切 mpv；用户手动关 mpv 窗口又切回 Plyr（`exitMpvMode`）
- 两套控制逻辑并存：键盘快捷键 mpv 分支 / Plyr 分支（`handleKeyboardShortcuts`）
- 依赖层面：`plyr`、`hls.js` 仍是 package.json 依赖，import 在播放页头部

### ② UI 承担 Resolver 职责（问题 B，确认存在）

`page.tsx` 中：

- `isDirectPlayableUrl()` 按扩展名判定资源类型（`page.tsx:382`）
- `resolveEpisodeUrl()` 直接 invoke `resolve_spider_episode` 并处理 raw id/flag/组
- `updateVideoUrl()` 决定“直接播 vs 先解析”，解析失败清空地址
- 网盘 dlink 失效重试逻辑（`directRetryRef`，`page.tsx:2641` video.onerror）
- `enrich_first_episode_direct` 首集直链化发生在 Rust，但其余集由前端逐集触发——解析职责被劈成两半，且两半逻辑各自独立实现（重复的 unwrap + wrap 序列）

### ③ UI 直连 mpv IPC（问题 D 前置，确认存在）

`mpv_embed_command` 在播放页出现 **9 处**调用，直接拼 mpv 原生命令数组：

- `['loadfile', url, 'replace', 'start=..']`（`page.tsx:529`）
- `['cycle','pause']`、`['seek',±10]`、`['add','volume',±5]`（快捷键，`page.tsx:2051`）
- `['seek', target, 'absolute']`（跳片头）、`['set_property','pause',true]`（跳片尾）
- DOM 遥控面板 4 处（`page.tsx:3399-3466`）

### ④ 双播放状态（问题 D，确认存在）

- Rust：mpv 进程 + observe_property（进程内事实）
- React：`mpvState {active,time,duration,paused,eof}` + `mpvLoadedUrlRef` + `suppressAutoMpvRef` + `mpvStateRef` —— 第二套状态，含换源去重、死循环抑制等业务决策
- 事件通道 `mpv-embed-event` 为 mpv 私有格式，非文档规定的 `playback_state` / `playback_time` 等

### ⑤ Gateway 缺口（问题 C，确认存在）

`netdisk_proxy.rs` 已具备：localhost-only、Range/Content-Range 透传、UA 注入、流式转发、随机端口。
对照 `04-gateway.md` 缺失：

- **无 opaque token**：真实直链 + UA 明文在 query string（`/netdisk/file.mp4?url=..&ua=..`），敏感信息暴露面大
- **无 ResourceSession / TTL**：无会话生命周期，无播放结束清理
- **无 SSRF 防护**：任意 http(s) 内层 URL 均代理
- **无 HEAD 支持**：只处理 GET（`handle_conn` 固定 `client().get`）
- **LAST_UA 全局兜底**是跨请求隐式状态，属权宜之计
- 定位是“网盘专用”，非通用资源访问层（普通源带 header 的 HLS 尚未受益——目前靠 fetch_m3u8 在 Rust 拉取规避）

### ⑥ video.rs 巨型模块

4121 行聚合：搜索、详情、播放器初始化聚合、解析、选源、进度、豆瓣、图片代理、m3u8 代理、缓存统计。文档已预判（“video.rs 不再承担所有业务逻辑”），现状确认。

### ⑦ `playback.rs` 名实不符

文档将 `crates/core/src/playback.rs` 视为 “Playback domain 天然边界”，但该文件实际内容是 `SkipDetection` + `filter_ads_from_m3_u8`，与播放器生命周期无关。Phase 4 建立 PlaybackManager 时需要决策：新建模块（如 `playback/` 目录）承载 PlaybackService，现有文件内容迁往 skip/adfilter，避免名字冲突。

### ⑧ 双 mpv 启动路径，其中一条已死

- `mpv_player.rs::launch_mpv`：一次性 spawn（无 IPC 控制，靠 mpv 单实例复用换源）。**前端已无任何调用**（grep src/ 零命中），仅 `mpv_available`/`locate_mpv` 被 `mpv_embed.rs` 复用。属遗留死代码，Phase 5/6 清理对象，现在不动。
- `mpv_embed.rs`：方案 C 活路径。

### ⑨ 播放历史由前端驱动

进度保存的节流决策在 Rust（player_tick），但触发、取值（Plyr currentTime 或 mpvState.time）、写入都由前端编排。目标架构下 HistoryService 应由 PlaybackManager 事件驱动。

### ⑩ 与文档的其他差异（如实记录，不改变架构）

- 文档说 `netdisk_proxy.rs` 是 Gateway 候选 —— 属实。
- 文档的 `ResolveEpisodeResponse.header` 目前返回给前端但基本未使用（仅日志），MediaResource 化后 headers 应留在 Rust 侧。
- 工程结构：仓库已是 workspace（`crates/core` 含 spider/bridge/netdisk 子模块），符合文档 06 “先保持现有目录，建立模块边界”的过渡形态，无需新建多个 crate。
- `initialize_player_view` 返回的 `PlayerInitialState` 是播放页 IPC 瘦身的既有成果，迁移时应保留该聚合入口形态。

## 4. 最小迁移方案（对照 07-migration.md 的落地映射）

原则：每阶段可编译、可回归；旧 command 签名不动，内部逐步换实现。

### Phase 1 — MediaResource（不改播放器、不改 UI）

- `crates/core/src/media.rs`（新）：`MediaResource` / `ResourceType` / `SubtitleResource` / `MediaMetadata`，按 `01-core-model.md`
- 转换入口：`RawPlayResult (url, header) → MediaResource`（spider/player.rs 输出处）、`直链 URL → MediaResource`
- TS 类型同步进 `src/lib/types.ts`（仅新增，不接线）
- 验收：MP4 / M3U8 / 网盘（含 UA）/ header 保留的转换单测

### Phase 2 — Resolver（video.rs 内部换芯，command 签名不动）

- `crates/core/src/resolver.rs`（或 resolver/ 目录）：`ResolveInput` / `Resolver` trait / `ResolverManager` / 错误模型（`02-resolver.md` 八类）
- 实现：DirectResolver（直链/扩展名判定，吃掉前端的 `isDirectPlayableUrl`）、SpiderResolver（吃掉 `resolve_spider_episode` 的 unwrap+wrap 序列）、NetdiskResolver、LocalFileResolver
- `video.rs::resolve_spider_episode` / `enrich_first_episode_direct` 改为委托 ResolverManager；前端不再做“直接播 vs 解析”判定（第二步 UI 清理时收口）
- 验收：同一 Episode 无论哪个 Source 均产出 MediaResource

### Phase 3 — Gateway（netdisk_proxy → PlaybackGateway，路径兼容）

- ResourceSession + opaque token（`/media/<token>`）+ TTL + 最近访问续期 + 播放结束清理
- 补 HEAD、SSRF 防护（协议/host 白名单、禁本地文件）、移除 LAST_UA 全局兜底（UA 入 session）
- 兼容期：`/netdisk/file.mp4?url=..` 旧路径继续可用，前端切换后删除
- 验收：Range/206/HEAD/Redirect/Header/Cookie/session TTL 测试

### Phase 4 — PlaybackManager（吸收 mpv_embed.rs）

- 新 playback 域模块：`PlaybackService` + `PlaybackState` + `MpvBackend`（现 mpv_embed.rs 的 spawn/pipe/observe 逻辑整体迁入，`playback.rs` 旧内容迁出更名）
- 新 IPC：`playback_play(MediaResource)` / `pause` / `seek` / `stop` / `set_volume`；事件改 `playback_state` / `playback_time` / `playback_duration` / `playback_error`（旧 `mpv-embed-event` 兼容期并存）
- Rust 成为播放状态唯一真相：`player_tick` 的进度/跳过决策改由 Rust 侧定时驱动（前端保留纯 UI 触发）
- 验收：load/play/pause/seek/stop/crash recovery

### Phase 5 — UI 清理（最后动 page.tsx）

- 删：resolve 逻辑、isDirectPlayableUrl、dlink 重试、mpv_embed_command 直调、mpvState 第二套状态、HLS/Plyr 路径与 HEVC 兜底分支
- 留：选集/换源/控制 UI、状态展示、Tauri IPC
- 删依赖：plyr、hls.js；删死代码：mpv_player.rs::launch_mpv
- 验收：点击选集→播放→进度→暂停→seek→下一集全链路

### Phase 6/7 — 稳定性与可选优化

超时、mpv 自动重启、session 清理、错误分类、日志（按 `10-testing.md` 的六个问题可回答）；libmpv embedding 继续冻结（ADR-004）。

## 5. 回归测试资源清单（Phase 0 登记）

- 本地 mp4 / 远程 mp4 直链
- 普通 HLS（含去广告开关）
- 网盘直链：百度（严格 UA）+ 夸克/UC，覆盖 dlink 过期重试场景
- HEVC-MKV 网盘源（触发 mpv 切换的场景，迁移后该场景成为唯一路径，需重点回归）
- Spider 搜索/详情/选集（桥接 APK 在环）
- 播放历史断点续播、跳过片头片尾、预载下一集

## 6. 结论

文档 `00-current-state.md` 对代码现状的判断**与实际一致**，无虚构代码；仅在 `playback.rs` 内容归属（⑦）和 mpv 双路径死代码（⑧）两处比文档预想的更明确。可按上述 Phase 1 开始实施。
