# Architecture Decision Records

## ADR-001: mpv 作为唯一播放后端

### Decision

采用 mpv 作为 QuantumTV 唯一正式播放引擎。

### Reason

TVBox/Spider 生态产生的资源类型复杂，包括 HLS、各种 HTTP 流、不同 Header/Cookie 要求、网盘临时地址等。mpv 对这些资源具有成熟的协议和解码支持。

### Consequence

WebView 不再作为视频播放后端。

---

## ADR-002: Resolver 与 Player 解耦

### Decision

所有资源首先转换为 MediaResource，再交给 PlaybackManager。

### Reason

避免不同 Source 对播放器产生直接依赖。

### Consequence

增加 Source/网盘时，不应修改播放器。

---

## ADR-003: Gateway 独立

### Decision

本地 HTTP Proxy 作为 Playback Gateway 独立模块。

### Reason

Proxy 解决的是资源访问问题，不是播放问题。

### Consequence

Gateway 可以独立测试。

---

## ADR-004: 第一阶段不使用 libmpv embedding

### Decision

先使用独立 mpv 进程 + JSON IPC。

### Reason

降低跨平台渲染、窗口句柄和 GPU 集成风险。

### Future

当核心架构稳定后再评估 libmpv。
