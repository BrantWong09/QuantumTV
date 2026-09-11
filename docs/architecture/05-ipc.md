# IPC Specification

## 1. UI -> Rust

Tauri Command 负责请求型操作。

例如：

```text
search
get_detail
get_episodes
resolve_episode
playback_play        (MediaResource JSON + start_at)
playback_pause
playback_set_paused
playback_stop
playback_seek        (secs + absolute)
playback_add_volume
playback_state       (快照兜底)
```

V2 Phase 4 已落地 `playback_*` 命令族 (src-tauri/src/commands/playback.rs)。
兼容期旧命令 `mpv_embed_launch/command/close` 委托到同一 PlaybackManager,
Phase 5 UI 切换后删除。

## 2. Rust -> UI

Tauri Event 推送状态 (Rust 侧唯一真相 PlaybackManager):

```text
playback_state       (完整快照: status/time/duration/resource_id/error)
playback_time        (time, 500ms 节流)
playback_duration
playback_error
```

兼容期同时翻译旧 `mpv-embed-event` (kind=time/duration/pause/eof/
file-loaded/dead), Phase 5 UI 切换后删除。

## 3. 不建议

不要让前端直接：

- 操作 mpv IPC socket
- 操作 Gateway
- 调用 Android Bridge
- 保存播放状态
- 拼接播放器参数

## 4. 数据边界

推荐：

```text
UI
 |
 | typed command
 v
Rust Core
 |
 | MediaResource
 v
PlaybackManager
 |
 | mpv protocol
 v
mpv
```

## 5. TypeScript 类型

Rust 输出的数据结构应该生成或维护对应 TypeScript 类型，避免 UI 自己猜字段。

例如：

```ts
export interface MediaResource {
  id: string
  url: string
  resourceType: string
  headers: Record<string, string>
  cookies: Record<string, string>
  userAgent?: string
  referer?: string
  proxyRequired: boolean
}
```
