# Playback Specification

## 1. 核心原则

QuantumTV 使用单一正式播放引擎：

> mpv

WebView HTML5 video 不再作为正式播放后端。

## 2. PlaybackManager

PlaybackManager 是 Rust Core 中唯一负责播放器控制的组件。

职责：

- 启动 mpv
- 销毁 mpv
- loadfile
- play
- pause
- stop
- seek
- volume
- subtitle
- track
- fullscreen
- 状态同步
- 错误处理

## 3. 播放流程

```text
MediaResource
    |
    v
PlaybackManager.play()
    |
    v
转换为 mpv 播放参数
    |
    v
mpv JSON IPC
```

## 4. 初期集成策略

第一阶段使用独立 mpv 进程：

```text
QuantumTV
    |
    +-- spawn mpv
    |
    +-- JSON IPC
```

不要第一阶段直接引入 libmpv。

原因：

- 降低跨平台窗口集成复杂度
- 降低 GPU/渲染耦合
- 更容易调试
- 更容易替换播放器
- 先验证业务架构

## 5. Player State

建议统一状态：

```rust
pub enum PlaybackState {
    Idle,
    Loading,
    Playing,
    Paused,
    Stopped,
    Ended,
    Error,
}
```

播放器状态只存在 Rust Core 一份。

UI 订阅 Rust 推送的状态。

不要让 React 自己维护第二套“真实播放状态”。

## 6. 事件

至少支持：

- state_changed
- time_changed
- duration_changed
- media_loaded
- playback_ended
- playback_error

## 7. 双播放器禁止

禁止出现：

```text
HTML5 Player -> fail -> mpv
```

应当始终：

```text
MediaResource -> PlaybackManager -> mpv
```
