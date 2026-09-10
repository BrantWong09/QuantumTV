# IPC Specification

## 1. UI -> Rust

Tauri Command 负责请求型操作。

例如：

```text
search
get_detail
get_episodes
resolve_episode
play
pause
stop
seek
set_volume
```

## 2. Rust -> UI

Tauri Event 推送状态：

```text
playback_state
playback_time
playback_duration
playback_error
```

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
