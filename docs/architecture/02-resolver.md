# Resolver Specification

## 1. Resolver 职责

Resolver 的唯一目标：

> 将某个 Episode/SourceResult 解析成 MediaResource。

Resolver 不负责：

- 播放
- UI
- mpv
- 播放进度
- React 状态

## 2. 接口

建议：

```rust
#[async_trait]
pub trait Resolver: Send + Sync {
    fn name(&self) -> &str;

    fn can_resolve(&self, input: &ResolveInput) -> bool;

    async fn resolve(
        &self,
        input: ResolveInput,
    ) -> Result<MediaResource, ResolveError>;
}
```

## 3. Resolver 类型

第一阶段：

- DirectResolver
- SpiderResolver
- AndroidSpiderResolver
- NetdiskResolver
- LocalFileResolver

## 4. Android Spider Bridge

Android Bridge 只负责执行 Spider，并取得：

```text
search
detail
play
```

尤其是播放阶段：

```text
playerContent()
    |
    v
RawPlayResult
    |
    v
AndroidSpiderResolver
    |
    v
MediaResource
```

Android Bridge 不允许直接调用 PlaybackManager。

## 5. Resolver Pipeline

```text
ResolveInput
    |
    v
ResolverManager
    |
    +-- candidate resolver
    |
    v
Resolver
    |
    v
Raw result
    |
    v
Normalize
    |
    v
Validate
    |
    v
MediaResource
```

## 6. 错误模型

至少区分：

- Unsupported
- NetworkError
- ParseError
- AuthenticationRequired
- ResourceExpired
- InvalidResource
- Timeout
- InternalError

错误应尽量携带 source、resolver 和可读 message。

## 7. 扩展原则

新增网盘时：

```text
新增 NetdiskResolver
```

而不是修改：

```text
Player
PlaybackManager
Next.js Player
```
