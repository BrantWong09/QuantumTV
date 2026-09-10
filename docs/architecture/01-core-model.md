# Core Model Specification

## 1. MediaResource

所有最终可播放资源必须标准化为 MediaResource。

```rust
pub struct MediaResource {
    pub id: String,
    pub url: String,
    pub resource_type: ResourceType,
    pub headers: std::collections::HashMap<String, String>,
    pub cookies: std::collections::HashMap<String, String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub proxy_required: bool,
    pub subtitles: Vec<SubtitleResource>,
    pub metadata: MediaMetadata,
}
```

## 2. ResourceType

```rust
pub enum ResourceType {
    File,
    Http,
    Hls,
    Dash,
    LocalFile,
    Unknown,
}
```

## 3. SubtitleResource

```rust
pub struct SubtitleResource {
    pub url: String,
    pub language: Option<String>,
    pub format: Option<String>,
}
```

## 4. MediaMetadata

至少包含：

```rust
pub struct MediaMetadata {
    pub title: Option<String>,
    pub episode: Option<String>,
    pub duration: Option<f64>,
}
```

## 5. 设计要求

MediaResource 必须满足：

- 与具体 Source 无关
- 与具体播放器无关
- 可序列化为 JSON
- TypeScript 与 Rust 类型保持一致
- 能表达 Header/Cookie/Referer
- 能表达是否需要本地代理
- 后续可扩展字幕和音轨

## 6. 禁止

不要把以下内容放入 MediaResource：

- TVBox 专有对象
- Android Context
- WebView 对象
- mpv handle
- UI 状态
- React state
- 数据库连接
