# QuantumTV V2 Current State and Migration Map

> 本文档基于 2026-09-10 上传的 QuantumTV 当前代码库整理。它描述“现状 → 目标”的映射，是 AI Coding Agent 开始重构前必须阅读的文档。

## 1. 当前总体判断

QuantumTV 已经具备目标架构的大部分能力：

- Tauri + Next.js 前端
- Rust Core
- TVBox/CMS 数据源
- Spider/Android Bridge
- 本地网盘 Proxy
- mpv 播放
- Plyr/HLS/WebView 播放路径
- 播放历史和进度

主要问题不是能力不足，而是职责重叠。

当前播放页同时承担：

```text
内容选择
  + Source/Spider resolve
  + HLS
  + HTML5/Plyr
  + mpv
  + 播放状态
  + 进度
  + 网盘资源处理
```

这造成多个“事实来源”，增加状态同步和生命周期问题。

## 2. 目标架构

```text
Next.js UI
    |
    | Tauri IPC
    v
Application Services
    |
    +-- ContentService
    +-- ResolveService
    +-- PlaybackService
    +-- HistoryService
    |
    +-----------------------------+
                                  |
                 +----------------+----------------+
                 |                                 |
             Source Layer                    Resolver Layer
                 |                                 |
        TVBox / CMS / Spider              Direct / Spider /
        Android Bridge                    Android / Netdisk
                 |                                 |
                 +----------------+----------------+
                                  |
                           MediaResource
                                  |
                         Playback Gateway
                                  |
                         PlaybackManager
                                  |
                                 mpv
```

## 3. 当前代码到目标模块的映射

### `src/app/play/page.tsx`

当前职责过多。

目标：

```text
保留：
- 选集 UI
- 换源 UI
- 播放控制 UI
- 播放状态展示

迁移：
- resolve → ResolveService
- mpv 控制 → PlaybackService
- 播放进度持久化 → HistoryService
- HLS/HTML5 播放 → 删除
```

最终页面应该主要是 UI + IPC 调用。

### `src-tauri/src/commands/video.rs`

当前是一个重要的兼容层，但职责偏大。

目标：

```text
保留：
- 旧 Tauri Command 的兼容入口

新增内部调用：
- ContentService
- ResolveService
- PlaybackService

最终：
video.rs 不再承担所有业务逻辑。
```

迁移期间不要一次删除旧 command。

### `src-tauri/src/commands/mpv_player.rs`

目标：

- 合并到 PlaybackManager/PlaybackService 的 mpv backend
- 保留必要的底层 mpv IPC 封装
- 不让 UI 直接依赖底层 mpv 命令

推荐：

```text
PlaybackService
    ↓
MpvPlayerBackend
    ↓
mpv JSON IPC
```

### `src-tauri/src/commands/mpv_embed.rs`

它属于播放器 backend / rendering integration。

第一阶段不要继续扩大职责。

目标：

```text
PlaybackManager
    ↓
MpvBackend
    ↓
mpv
```

如果当前 embedded mpv 不稳定，优先允许独立 mpv 进程模式作为 V2 初始实现。

### `crates/core/src/netdisk_proxy.rs`

不要删除。

目标名称/职责：

```text
PlaybackGateway
```

从“网盘 Proxy”升级为通用资源访问 Gateway。

保留能力：

- Range
- Header
- Cookie
- Referer
- User-Agent
- Redirect
- 临时资源

新增：

- opaque token
- ResourceSession
- TTL
- localhost-only
- SSRF 防护
- 本地文件访问限制

### `crates/core/src/types.rs`

目前属于内容领域模型。

不要直接把播放器状态塞进去。

建议逐步形成：

```text
Content Models
- SearchResult
- Detail
- PlayGroup
- Episode

Media Models
- MediaResource
- SubtitleResource
- MediaMetadata
```

### `crates/core/src/playback.rs`

这是天然的 Playback Domain 边界。

目标：

```text
PlaybackService
PlaybackState
PlaybackSession
```

播放器实现细节不要继续扩散到其他模块。

## 4. 当前主要架构问题

### 问题 A：双播放器

当前同时存在：

```text
Plyr / HTML5 / HLS
        +
       mpv
```

目标：

```text
MediaResource
      ↓
PlaybackService
      ↓
mpv
```

### 问题 B：播放页承担资源解析

目标：

```text
Episode
  ↓
ResolveService
  ↓
MediaResource
  ↓
PlaybackService
```

### 问题 C：网盘 Proxy 与播放器逻辑耦合

目标：

```text
Resolver
  ↓
MediaResource
  ↓
Gateway（必要时）
  ↓
mpv
```

### 问题 D：播放状态存在多个来源

目标：

```text
mpv
 ↓
PlaybackManager
 ↓
Tauri Event
 ↓
Next.js UI
```

Rust 是状态真相来源。

## 5. 重构原则

- 不推倒重写。
- 不同时修改 Source、Resolver、Player 三层。
- 每个阶段保持可编译。
- 每阶段有回归测试。
- 新功能必须进入正确模块。
- 新增网盘不能修改 Player。
- 新增 Source 不能修改 Player。
- 播放器不能知道 TVBox/Spider 的实现细节。
