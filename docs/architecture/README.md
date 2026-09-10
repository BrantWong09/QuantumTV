# QuantumTV V2 Architecture

## 文档用途

本目录是 QuantumTV 下一阶段重构的唯一架构参考。

AI Coding Agent 必须先阅读：

1. `README.md`
2. `00-current-state.md`
3. `08-ai-implementation.md`
4. `07-migration.md`

然后根据具体任务阅读对应文档。

## 核心架构

```text
TVBox / CMS / Spider / Android Bridge / Netdisk
                    |
                    v
             Resolver Manager
                    |
                    v
              MediaResource
                    |
                    v
            Playback Gateway
                    |
                    v
             PlaybackManager
                    |
                    v
                   mpv
```

## 最重要的边界

```text
Source
  = 找资源

Resolver
  = 把资源解析成统一 MediaResource

MediaResource
  = 描述“怎么访问一个媒体资源”

Gateway
  = 解决 Header/Cookie/Range/Redirect/临时 URL 等访问问题

PlaybackManager
  = 控制播放器

mpv
  = 解码和播放

UI
  = 展示和用户交互
```

## 绝对规则

1. mpv 是唯一正式播放后端。
2. 不再保留 WebView/Plyr/HLS 与 mpv 的双播放器架构。
3. Resolver 不得调用播放器。
4. Source 不得调用播放器。
5. UI 不得直接调用 mpv IPC。
6. 所有播放入口必须经过 MediaResource。
7. 网盘属于 Resolver/资源访问问题，不属于 Player。
8. Playback Gateway 是通用资源访问层，不只服务网盘。
9. Rust PlaybackManager 是播放状态真相来源。
10. 重构采用增量迁移，不推倒重写。

## 当前代码重点

- `src/app/play/page.tsx`: 当前最大耦合点
- `src-tauri/src/commands/video.rs`: 当前大型兼容/业务入口
- `src-tauri/src/commands/mpv_player.rs`: mpv backend 候选
- `src-tauri/src/commands/mpv_embed.rs`: embedded 播放实现
- `crates/core/src/netdisk_proxy.rs`: PlaybackGateway 候选
- `crates/core/src/types.rs`: 内容模型
- `crates/core/src/playback.rs`: Playback domain 候选

详见 `00-current-state.md`。

## 文档地图

| 文件 | 用途 |
|---|---|
| 00-current-state.md | 基于当前代码的现状和迁移映射 |
| 01-core-model.md | MediaResource |
| 02-resolver.md | Resolver |
| 03-playback.md | mpv / PlaybackManager |
| 04-gateway.md | Playback Gateway |
| 05-ipc.md | Tauri IPC |
| 06-project-structure.md | 模块结构 |
| 07-migration.md | 分阶段迁移 |
| 08-ai-implementation.md | AI 实施规则 |
| 09-decisions.md | 架构决策 |
| 10-testing.md | 测试策略 |
| 11-refactor-checklist.md | 可执行重构清单 |
| 12-agent-prompts.md | AI Coding Agent 提示词 |
