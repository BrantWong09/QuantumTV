# Testing Strategy

## 1. Resolver Unit Tests

每种 Resolver 至少测试：

- 正常资源
- 无效资源
- 超时
- Header/Cookie
- 过期 URL

## 2. Gateway Tests

必须覆盖：

- GET
- HEAD
- Range
- 206
- Content-Range
- Redirect
- Header 转发
- Cookie 转发
- session TTL

## 3. Playback Tests

PlaybackManager 至少验证：

- start
- load
- pause
- resume
- seek
- stop
- mpv crash
- mpv exit
- playback end

## 4. Integration Tests

建立至少以下测试资源类型：

```text
local mp4
remote mp4
HLS
需要 Header 的 HLS
需要 Cookie 的资源
网盘代理资源
```

## 5. 回归测试

重构期间必须保持：

- TVBox 搜索
- TVBox 详情
- Spider
- Android Bridge
- 选集
- 播放历史

可用。

## 6. 日志

日志应该能够回答：

```text
哪个 Source？
哪个 Episode？
哪个 Resolver？
是否经过 Gateway？
最终 Resource 类型？
mpv 是否启动？
mpv 返回什么错误？
```

避免打印完整 Cookie、Authorization 或其他敏感信息。
