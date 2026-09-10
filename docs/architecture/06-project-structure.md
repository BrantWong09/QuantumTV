# Recommended Project Structure

目标不是一次性机械移动所有代码，而是逐步建立边界。

建议最终结构：

```text
quantumtv/
├── apps/
│   └── desktop/
│       └── nextjs/
│
├── crates/
│   ├── core/
│   │   ├── source/
│   │   ├── resolver/
│   │   ├── media/
│   │   ├── playback/
│   │   ├── gateway/
│   │   └── history/
│   │
│   ├── tvbox/
│   ├── spider/
│   ├── android-bridge/
│   └── storage/
│
├── src-tauri/
│   ├── commands/
│   ├── events/
│   └── main.rs
│
└── docs/
    ├── architecture/
    └── development/
```

如果当前仓库规模还不适合 workspace，可以先保持现有目录，仅在 `src-tauri` 内建立模块边界。

## 依赖方向

必须尽量保持：

```text
UI
 ↓
Tauri Commands
 ↓
Core
 ↓
Source / Resolver
 ↓
Media
 ↓
Playback / Gateway
```

禁止：

```text
UI -> mpv
Resolver -> UI
Spider -> mpv
Spider -> React
Gateway -> UI
```

## 模块职责

### source

描述资源来源。

### resolver

把来源转换为 MediaResource。

### media

定义统一资源模型。

### playback

播放器生命周期和控制。

### gateway

本地 HTTP 代理。

### history

播放历史和进度。

### storage

配置和持久化。
