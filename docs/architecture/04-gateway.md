# Playback Gateway Specification

## 1. 定位

Playback Gateway 是本地 HTTP 资源访问层。

它不是播放器，也不是 Resolver。

主要解决：

- Header 注入
- Cookie 注入
- Referer
- User-Agent
- Range
- Redirect
- 网盘临时 URL
- 某些播放器无法直接访问的资源

## 2. 典型流程

```text
MediaResource
    |
    | proxy_required=true
    v
PlaybackGateway
    |
    v
127.0.0.1:<port>/media/<token>
    |
    v
真实远端资源
```

如果资源可以被 mpv 直接访问：

```text
MediaResource
    |
    v
mpv
```

无需 Gateway。

## 3. Token

不要把完整真实 URL、Cookie 等敏感信息直接放在 URL query 中。

推荐：

```text
/media/<opaque-token>
```

Token 映射到内存中的 ResourceSession。

## 4. Range

Gateway 必须正确支持：

- Range
- Content-Range
- Accept-Ranges
- 206 Partial Content
- HEAD
- Content-Length

## 5. 生命周期

ResourceSession 应有：

- 创建时间
- 最近访问时间
- TTL
- 失效机制

播放结束后主动清理。

## 6. 安全

Gateway 默认只监听 localhost。

禁止无认证地暴露到局域网。

需要防止：

- SSRF
- 任意 URL 代理
- 本地文件读取
- token 猜测
- header 注入
