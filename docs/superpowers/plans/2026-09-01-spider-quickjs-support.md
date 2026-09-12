# TVBox Spider QuickJS 支持实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 QuantumTV 中嵌入 QuickJS 引擎，支持执行 `.js` 格式的 TVBox Spider 插件，实现搜索和播放功能。

**Architecture:** 在 `crates/core/src/spider/` 下新增 Spider 模块，包含 Dispatcher（路由）、QuickJS Engine（执行）、Host API（桥接）。修改 `video.rs` 的搜索/播放逻辑，通过 Dispatcher 路由到对应引擎。

**Tech Stack:** Rust, rquickjs (QuickJS binding), reqwest (HTTP), tokio (async)

**Spec:** `docs/superpowers/specs/2026-09-01-spider-quickjs-support-design.md`

## Global Constraints

- Rust 1.90+
- rquickjs 版本 0.12.x
- 遵循现有代码风格（无 `unwrap()` 在生产代码中，使用 `tracing` 日志）
- 测试命令：`cargo test -p quantumtv-core`
- 提交前运行：`cargo fmt && cargo clippy`

---

## 文件结构

| 文件 | 操作 | 职责 |
|------|------|------|
| `crates/core/Cargo.toml` | 修改 | 添加 rquickjs 依赖 |
| `crates/core/src/lib.rs` | 修改 | 导出 spider 模块 |
| `crates/core/src/spider/mod.rs` | 创建 | Spider trait + 公共类型导出 |
| `crates/core/src/spider/types.rs` | 创建 | JS ↔ Rust 数据结构 |
| `crates/core/src/spider/host_api.rs` | 创建 | $api 宿主对象实现 |
| `crates/core/src/spider/quickjs_engine.rs` | 创建 | QuickJS 引擎封装 |
| `crates/core/src/spider/dispatcher.rs` | 创建 | 路由逻辑 |
| `src-tauri/src/commands/video.rs` | 修改 | 搜索/播放调用 Dispatcher |

---

### Task 1: 项目设置与依赖配置

**Files:**
- Modify: `crates/core/Cargo.toml`

**Interfaces:**
- Produces: rquickjs 依赖可用

- [ ] **Step 1: 添加 rquickjs 依赖到 Cargo.toml**

在 `crates/core/Cargo.toml` 的 `[dependencies]` 中添加：

```toml
rquickjs = { version = "0.12", features = ["async", "macro"] }
```

- [ ] **Step 2: 验证编译**

Run: `cargo check -p quantumtv-core`
Expected: 编译成功（可能有 warning，无 error）

- [ ] **Step 3: Commit**

```bash
git add crates/core/Cargo.toml
git commit -m "chore(core): add rquickjs dependency for Spider support"
```

---

### Task 2: Spider 类型定义

**Files:**
- Create: `crates/core/src/spider/types.rs`

**Interfaces:**
- Produces: `SpiderSearchResult`, `SpiderDetailResult`, `SpiderPlayerResult` 类型

- [ ] **Step 1: 创建 spider 模块目录**

```bash
mkdir -p crates/core/src/spider
```

- [ ] **Step 2: 创建 types.rs 文件**

```rust
use serde::{Deserialize, Serialize};

/// Spider 搜索结果中的单个视频项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpiderVodItem {
    #[serde(rename = "vod_id")]
    pub vod_id: String,
    #[serde(rename = "vod_name")]
    pub vod_name: String,
    #[serde(rename = "vod_pic", skip_serializing_if = "Option::is_none")]
    pub vod_pic: Option<String>,
    #[serde(rename = "vod_remarks", skip_serializing_if = "Option::is_none")]
    pub vod_remarks: Option<String>,
    #[serde(rename = "type_name", skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(rename = "vod_year", skip_serializing_if = "Option::is_none")]
    pub vod_year: Option<String>,
    #[serde(rename = "vod_area", skip_serializing_if = "Option::is_none")]
    pub vod_area: Option<String>,
    #[serde(rename = "vod_content", skip_serializing_if = "Option::is_none")]
    pub vod_content: Option<String>,
    #[serde(rename = "vod_actor", skip_serializing_if = "Option::is_none")]
    pub vod_actor: Option<String>,
    #[serde(rename = "vod_director", skip_serializing_if = "Option::is_none")]
    pub vod_director: Option<String>,
    #[serde(rename = "vod_play_from", skip_serializing_if = "Option::is_none")]
    pub vod_play_from: Option<String>,
    #[serde(rename = "vod_play_url", skip_serializing_if = "Option::is_none")]
    pub vod_play_url: Option<String>,
}

/// Spider 分类
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpiderClass {
    #[serde(rename = "type_id")]
    pub type_id: String,
    #[serde(rename = "type_name")]
    pub type_name: String,
}

/// Spider 搜索响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpiderSearchResponse {
    #[serde(default)]
    pub list: Vec<SpiderVodItem>,
}

/// Spider 详情响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpiderDetailResponse {
    #[serde(default)]
    pub list: Vec<SpiderVodItem>,
}

/// Spider 播放响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpiderPlayerResponse {
    /// 是否需要解析 (0=直接播放, 1=需要解析)
    #[serde(default)]
    pub parse: i32,
    /// 直接播放 URL
    #[serde(rename = "playUrl", skip_serializing_if = "Option::is_none")]
    pub play_url: Option<String>,
    /// 播放 URL (备选)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// 自定义请求头
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<serde_json::Value>,
    /// 格式 (如 "m3u8")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// 加密类型
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypt: Option<i32>,
}

/// QuickJS fetch 请求选项
#[derive(Debug, Clone, Deserialize)]
pub struct FetchOptions {
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_method() -> String {
    "GET".to_string()
}

fn default_timeout() -> u64 {
    30000
}

/// QuickJS fetch 响应
#[derive(Debug, Clone, Serialize)]
pub struct FetchResponse {
    pub status_code: u16,
    pub body: String,
    pub headers: std::collections::HashMap<String, String>,
}
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p quantumtv-core`
Expected: 编译成功

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/spider/types.rs
git commit -m "feat(spider): add type definitions for JS Spider data structures"
```

---

### Task 3: Spider Trait 定义

**Files:**
- Create: `crates/core/src/spider/mod.rs`

**Interfaces:**
- Consumes: types.rs 中的类型
- Produces: `SpiderEngine` trait

- [ ] **Step 1: 创建 mod.rs 文件**

```rust
pub mod types;

use async_trait::async_trait;
use types::*;

/// Spider 引擎 trait
///
/// 所有 Spider 引擎（CMS、QuickJS 等）都实现此 trait
#[async_trait]
pub trait SpiderEngine: Send + Sync {
    /// 获取引擎名称
    fn name(&self) -> &str;

    /// 搜索视频
    async fn search(&self, query: &str, quick: bool) -> Result<Vec<SpiderVodItem>, String>;

    /// 获取视频详情
    async fn detail(&self, ids: &[String]) -> Result<Vec<SpiderVodItem>, String>;

    /// 获取播放信息
    async fn player(
        &self,
        flag: &str,
        id: &str,
        vip_flags: &[String],
    ) -> Result<SpiderPlayerResponse, String>;

    /// 获取首页内容 (可选实现)
    async fn home_content(&self) -> Result<String, String> {
        Err("not implemented".to_string())
    }

    /// 获取分类内容 (可选实现)
    async fn category_content(
        &self,
        _tid: &str,
        _pg: i32,
        _filter: &str,
        _extend: &str,
    ) -> Result<String, String> {
        Err("not implemented".to_string())
    }
}
```

- [ ] **Step 2: 在 lib.rs 中导出 spider 模块**

在 `crates/core/src/lib.rs` 末尾添加：

```rust
pub mod spider;
```

- [ ] **Step 3: 添加 async-trait 依赖**

在 `crates/core/Cargo.toml` 的 `[dependencies]` 中添加：

```toml
async-trait = "0.1"
```

- [ ] **Step 4: 验证编译**

Run: `cargo check -p quantumtv-core`
Expected: 编译成功

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/spider/mod.rs crates/core/src/lib.rs crates/core/Cargo.toml
git commit -m "feat(spider): add SpiderEngine trait definition"
```

---

### Task 4: Host API 桥接实现

**Files:**
- Create: `crates/core/src/spider/host_api.rs`

**Interfaces:**
- Consumes: rquickjs, types.rs
- Produces: `setup_host_api()` 函数，注入 $api 到 JS 环境

- [ ] **Step 1: 创建 host_api.rs 文件**

```rust
use rquickjs::{Ctx, Function, Object};
use std::collections::HashMap;

use super::types::{FetchOptions, FetchResponse};

/// 设置宿主 API 到 JS 环境
pub fn setup_host_api(ctx: &Ctx<'_>, ext: &str) -> Result<(), rquickjs::Error> {
    let globals = ctx.globals();
    let api = Object::new(ctx.clone())?;

    // 注入 ext 值
    let ext_str = ext.to_string();
    let get_ext_fn = Function::new(ctx.clone(), move || -> String {
        ext_str.clone()
    })?;
    api.set("getExt", get_ext_fn)?;

    // 注入 log
    let log_fn = Function::new(ctx.clone(), |msg: String| {
        tracing::info!("[Spider JS] {}", msg);
    })?;
    api.set("log", log_fn)?;

    // 注入 error
    let error_fn = Function::new(ctx.clone(), |msg: String| {
        tracing::error!("[Spider JS] {}", msg);
    })?;
    api.set("error", error_fn)?;

    // 注入 base64Encode
    let base64_encode_fn = Function::new(ctx.clone(), |input: String| {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
    })?;
    api.set("base64Encode", base64_encode_fn)?;

    // 注入 base64Decode
    let base64_decode_fn = Function::new(ctx.clone(), |input: String| -> Result<String, String> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(input.as_bytes())
            .map_err(|e| e.to_string())?;
        String::from_utf8(bytes).map_err(|e| e.to_string())
    })?;
    api.set("base64Decode", base64_decode_fn)?;

    // 注入 urlEncode
    let url_encode_fn = Function::new(ctx.clone(), |input: String| {
        urlencoding::encode(&input).to_string()
    })?;
    api.set("urlEncode", url_encode_fn)?;

    // 注入 urlDecode
    let url_decode_fn = Function::new(ctx.clone(), |input: String| -> Result<String, String> {
        urlencoding::decode(&input)
            .map(|s| s.to_string())
            .map_err(|e| e.to_string())
    })?;
    api.set("urlDecode", url_decode_fn)?;

    // 注入 fetch (简化版，同步实现)
    // 注意：rquickjs 的 async 支持需要 AsyncContext，这里用同步包装
    let fetch_fn = Function::new(ctx.clone(), |url: String, opts: Option<FetchOptions>| -> Result<FetchResponse, String> {
        let opts = opts.unwrap_or_default();
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(opts.timeout))
            .build()
            .map_err(|e| e.to_string())?;

        let mut builder = match opts.method.to_uppercase().as_str() {
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            _ => client.get(&url),
        };

        for (key, value) in &opts.headers {
            builder = builder.header(key.as_str(), value.as_str());
        }

        if let Some(body) = &opts.body {
            builder = builder.body(body.clone());
        }

        let response = builder.send().map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let headers: HashMap<String, String> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = response.text().map_err(|e| e.to_string())?;

        Ok(FetchResponse {
            status_code: status,
            body,
            headers,
        })
    })?;
    api.set("fetch", fetch_fn)?;

    globals.set("$api", api)?;

    Ok(())
}

/// 同步版本的 fetch 调用（用于非 async 上下文）
pub fn fetch_sync(
    url: &str,
    method: &str,
    headers: &HashMap<String, String>,
    body: Option<&str>,
    timeout_ms: u64,
) -> Result<FetchResponse, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| e.to_string())?;

    let mut builder = match method.to_uppercase().as_str() {
        "POST" => client.post(url),
        "PUT" => client.put(url),
        "DELETE" => client.delete(url),
        _ => client.get(url),
    };

    for (key, value) in headers {
        builder = builder.header(key.as_str(), value.as_str());
    }

    if let Some(b) = body {
        builder = builder.body(b.to_string());
    }

    let response = builder.send().map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let resp_headers: HashMap<String, String> = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = response.text().map_err(|e| e.to_string())?;

    Ok(FetchResponse {
        status_code: status,
        body,
        headers: resp_headers,
    })
}
```

- [ ] **Step 2: 在 mod.rs 中导出 host_api**

在 `crates/core/src/spider/mod.rs` 中添加：

```rust
pub mod host_api;
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p quantumtv-core`
Expected: 编译成功

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/spider/host_api.rs crates/core/src/spider/mod.rs
git commit -m "feat(spider): implement host API bridge for QuickJS"
```

---

### Task 5: QuickJS 引擎实现

**Files:**
- Create: `crates/core/src/spider/quickjs_engine.rs`

**Interfaces:**
- Consumes: rquickjs, host_api.rs, types.rs
- Produces: `QuickJsEngine` struct 实现 `SpiderEngine` trait

- [ ] **Step 1: 创建 quickjs_engine.rs 文件**

```rust
use async_trait::async_trait;
use rquickjs::{Context, Function, Runtime};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

use super::host_api::setup_host_api;
use super::types::*;
use super::SpiderEngine;

/// QuickJS Spider 引擎
pub struct QuickJsEngine {
    /// JS 源码
    js_source: String,
    /// ext 配置参数
    ext: String,
    /// JS 文件 URL（用于缓存 key）
    url: String,
}

impl QuickJsEngine {
    /// 从 URL 创建引擎
    pub async fn from_url(url: &str, ext: &str) -> Result<Self, String> {
        let js_source = download_js(url).await?;
        Ok(Self {
            js_source,
            ext: ext.to_string(),
            url: url.to_string(),
        })
    }

    /// 从本地文件创建引擎
    pub fn from_file(path: &str, ext: &str) -> Result<Self, String> {
        let js_source = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read JS file: {}", e))?;
        Ok(Self {
            js_source,
            ext: ext.to_string(),
            url: path.to_string(),
        })
    }

    /// 创建 QuickJS 运行时并执行 JS 方法
    async fn call_js_method(&self, method: &str, args: Vec<String>) -> Result<String, String> {
        let js_source = self.js_source.clone();
        let ext = self.ext.clone();

        // 在 blocking 线程中执行 QuickJS（因为它不是 async safe 的）
        tokio::task::spawn_blocking(move || {
            let rt = Runtime::new().map_err(|e| format!("Failed to create runtime: {}", e))?;
            let ctx = Context::full(&rt).map_err(|e| format!("Failed to create context: {}", e))?;

            ctx.with(|ctx| {
                // 设置宿主 API
                setup_host_api(&ctx, &ext)
                    .map_err(|e| format!("Failed to setup host API: {}", e))?;

                // 执行 JS 源码
                ctx.eval(&js_source)
                    .map_err(|e| format!("Failed to eval JS: {}", e))?;

                // 获取 spider 对象
                let globals = ctx.globals();
                let spider: rquickjs::Object = globals
                    .get("spider")
                    .map_err(|e| format!("Failed to get spider: {}", e))?;

                // 调用方法
                let func: Function = spider
                    .get(method)
                    .map_err(|e| format!("Failed to get method {}: {}", method, e))?;

                // 构造参数
                let js_args: Vec<rquickjs::Value> = args
                    .iter()
                    .map(|a| {
                        rquickjs::Value::from_string(ctx.clone(), a)
                            .map_err(|e| e.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("Failed to create args: {}", e))?;

                // 调用并获取结果
                let result: rquickjs::Value = func
                    .call(js_args)
                    .map_err(|e| format!("Failed to call {}: {}", method, e))?;

                // 转换结果为字符串
                let result_str: String = result
                    .to_string()
                    .map_err(|e| format!("Failed to convert result: {}", e))?;

                Ok(result_str)
            })
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }
}

/// 下载 JS 文件
async fn download_js(url: &str) -> Result<String, String> {
    // 检查磁盘缓存
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("quantumtv")
        .join("spiders");

    let url_hash = format!("{:x}", md5::compute(url.as_bytes()));
    let cache_path = cache_dir.join(format!("{}.js", url_hash));

    // 检查缓存是否有效（24小时）
    if cache_path.exists() {
        if let Ok(metadata) = std::fs::metadata(&cache_path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(elapsed) = modified.elapsed() {
                    if elapsed.as_secs() < 86400 {
                        return std::fs::read_to_string(&cache_path)
                            .map_err(|e| format!("Failed to read cached JS: {}", e));
                    }
                }
            }
        }
    }

    // 下载
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to create client: {}", e))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to download JS: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }

    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    // 保存到缓存
    let _ = std::fs::create_dir_all(&cache_dir);
    let _ = std::fs::write(&cache_path, &body);

    Ok(body)
}

#[async_trait]
impl SpiderEngine for QuickJsEngine {
    fn name(&self) -> &str {
        "QuickJS"
    }

    async fn search(&self, query: &str, quick: bool) -> Result<Vec<SpiderVodItem>, String> {
        let result = self
            .call_js_method("searchContent", vec![query.to_string(), quick.to_string()])
            .await?;

        let response: SpiderSearchResponse =
            serde_json::from_str(&result).map_err(|e| format!("Failed to parse search result: {}", e))?;

        Ok(response.list)
    }

    async fn detail(&self, ids: &[String]) -> Result<Vec<SpiderVodItem>, String> {
        let ids_str = ids.join(",");
        let result = self
            .call_js_method("detailContent", vec![ids_str])
            .await?;

        let response: SpiderDetailResponse =
            serde_json::from_str(&result).map_err(|e| format!("Failed to parse detail result: {}", e))?;

        Ok(response.list)
    }

    async fn player(
        &self,
        flag: &str,
        id: &str,
        vip_flags: &[String],
    ) -> Result<SpiderPlayerResponse, String> {
        let vip_flags_str = serde_json::to_string(vip_flags)
            .map_err(|e| format!("Failed to serialize vip_flags: {}", e))?;

        let result = self
            .call_js_method("playerContent", vec![flag.to_string(), id.to_string(), vip_flags_str])
            .await?;

        let response: SpiderPlayerResponse = serde_json::from_str(&result)
            .map_err(|e| format!("Failed to parse player result: {}", e))?;

        Ok(response)
    }
}
```

- [ ] **Step 2: 在 mod.rs 中导出 quickjs_engine**

在 `crates/core/src/spider/mod.rs` 中添加：

```rust
pub mod quickjs_engine;
```

- [ ] **Step 3: 添加 dirs 依赖**

在 `crates/core/Cargo.toml` 的 `[dependencies]` 中添加：

```toml
dirs = "5"
```

- [ ] **Step 4: 验证编译**

Run: `cargo check -p quantumtv-core`
Expected: 编译成功

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/spider/quickjs_engine.rs crates/core/src/spider/mod.rs crates/core/Cargo.toml
git commit -m "feat(spider): implement QuickJS Spider engine"
```

---

### Task 6: Spider Dispatcher 实现

**Files:**
- Create: `crates/core/src/spider/dispatcher.rs`

**Interfaces:**
- Consumes: SpiderEngine trait, QuickJsEngine
- Produces: `SpiderDispatcher` struct，`dispatch()` 函数

- [ ] **Step 1: 创建 dispatcher.rs 文件**

```rust
use super::quickjs_engine::QuickJsEngine;
use super::types::*;
use super::SpiderEngine;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Spider 分发器
///
/// 根据站点配置路由到对应的引擎
pub struct SpiderDispatcher {
    /// 已缓存的引擎实例 (key: js_url)
    engines: RwLock<HashMap<String, Arc<QuickJsEngine>>>,
}

impl SpiderDispatcher {
    pub fn new() -> Self {
        Self {
            engines: RwLock::new(HashMap::new()),
        }
    }

    /// 判断是否为 JS Spider 站点
    pub fn is_js_spider(site_type: i32, api: &str) -> bool {
        if site_type != 3 {
            return false;
        }
        // JS Spider 的 api 字段通常是 URL 以 .js 结尾，或包含 .js?
        api.ends_with(".js") || api.contains(".js?") || api.ends_with(".js/")
    }

    /// 获取或创建引擎
    async fn get_engine(&self, api: &str, ext: &str) -> Result<Arc<QuickJsEngine>, String> {
        // 检查缓存
        {
            let engines = self.engines.read().await;
            if let Some(engine) = engines.get(api) {
                return Ok(engine.clone());
            }
        }

        // 创建新引擎
        let engine = QuickJsEngine::from_url(api, ext).await?;
        let engine = Arc::new(engine);

        // 缓存
        {
            let mut engines = self.engines.write().await;
            engines.insert(api.to_string(), engine.clone());
        }

        Ok(engine)
    }

    /// 搜索视频
    pub async fn search(
        &self,
        site_type: i32,
        api: &str,
        ext: &str,
        query: &str,
        quick: bool,
    ) -> Result<Vec<SpiderVodItem>, String> {
        if !Self::is_js_spider(site_type, api) {
            return Err(format!("Not a JS spider site: api={}, type={}", api, site_type));
        }

        let engine = self.get_engine(api, ext).await?;
        engine.search(query, quick).await
    }

    /// 获取视频详情
    pub async fn detail(
        &self,
        site_type: i32,
        api: &str,
        ext: &str,
        ids: &[String],
    ) -> Result<Vec<SpiderVodItem>, String> {
        if !Self::is_js_spider(site_type, api) {
            return Err(format!("Not a JS spider site: api={}, type={}", api, site_type));
        }

        let engine = self.get_engine(api, ext).await?;
        engine.detail(ids).await
    }

    /// 获取播放信息
    pub async fn player(
        &self,
        site_type: i32,
        api: &str,
        ext: &str,
        flag: &str,
        id: &str,
        vip_flags: &[String],
    ) -> Result<SpiderPlayerResponse, String> {
        if !Self::is_js_spider(site_type, api) {
            return Err(format!("Not a JS spider site: api={}, type={}", api, site_type));
        }

        let engine = self.get_engine(api, ext).await?;
        engine.player(flag, id, vip_flags).await
    }
}

impl Default for SpiderDispatcher {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 2: 在 mod.rs 中导出 dispatcher**

在 `crates/core/src/spider/mod.rs` 中添加：

```rust
pub mod dispatcher;
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p quantumtv-core`
Expected: 编译成功

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/spider/dispatcher.rs crates/core/src/spider/mod.rs
git commit -m "feat(spider): implement Spider Dispatcher for routing"
```

---

### Task 7: 集成到搜索流程

**Files:**
- Modify: `src-tauri/src/commands/video.rs`

**Interfaces:**
- Consumes: SpiderDispatcher
- Produces: 搜索结果中包含 Spider 站点的结果

- [ ] **Step 1: 在 video.rs 中添加 Spider 导入**

在 `src-tauri/src/commands/video.rs` 顶部添加：

```rust
use quantumtv_core::spider::dispatcher::SpiderDispatcher;
```

- [ ] **Step 2: 在 SearchCacheManager 同级添加 SpiderDispatcher 状态**

在 `src-tauri/src/lib.rs` 中注册状态（如果还没有的话）：

```rust
.manage(SpiderDispatcher::new())
```

- [ ] **Step 3: 修改 search_with_cache_hit 函数**

在 `search_with_cache_hit` 函数中，在 sites 过滤后添加 Spider 搜索逻辑：

```rust
// 在搜索现有 CMS 源的同时，搜索 JS Spider 源
let spider_dispatcher = app_handle.state::<SpiderDispatcher>();

// 收集 JS Spider 站点
let js_spider_sites: Vec<_> = sites.iter().filter(|s| {
    SpiderDispatcher::is_js_spider(3, &s.api) // type=3 且 api 是 JS URL
}).collect();

// 并发搜索 JS Spider 源
let mut spider_handles = Vec::new();
for site in &js_spider_sites {
    let dispatcher = spider_dispatcher.inner().clone();
    let api = site.api.clone();
    let name = site.name.clone();
    let key = site.key.clone();
    let query = query.clone();
    let app_handle = app_handle.clone();

    let handle = tokio::spawn(async move {
        // 从配置中获取 ext
        let ext = String::new(); // TODO: 从 site 配置中获取 ext

        match dispatcher.search(3, &api, &ext, &query, false).await {
            Ok(items) => {
                items.into_iter().map(move |item| {
                    SearchResult {
                        id: item.vod_id,
                        title: item.vod_name,
                        poster: item.vod_pic.unwrap_or_default(),
                        episodes: vec![], // Spider 结果的集数在 detail 中获取
                        episodes_titles: vec![],
                        source: key.clone(),
                        source_name: name.clone(),
                        class: item.type_name,
                        year: item.vod_year,
                        desc: item.vod_content,
                        type_name: item.type_name,
                        douban_id: None,
                    }
                }).collect::<Vec<_>>()
            }
            Err(e) => {
                tracing::warn!("Spider search failed for {}: {}", name, e);
                vec![]
            }
        }
    });
    spider_handles.push(handle);
}

// 等待所有 Spider 搜索完成
let spider_results: Vec<SearchResult> = futures::future::join_all(spider_handles)
    .await
    .into_iter()
    .filter_map(|r| r.ok())
    .flatten()
    .collect();

// 合并结果
all_results.extend(spider_results);
```

- [ ] **Step 4: 添加 futures 依赖**

在 `src-tauri/Cargo.toml` 中添加：

```toml
futures = "0.3"
```

- [ ] **Step 5: 验证编译**

Run: `cargo check -p quantumtv`
Expected: 编译成功

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/commands/video.rs src-tauri/Cargo.toml
git commit -m "feat: integrate Spider search into video search flow"
```

---

### Task 8: 集成到播放流程

**Files:**
- Modify: `src-tauri/src/commands/video.rs`

**Interfaces:**
- Consumes: SpiderDispatcher
- Produces: 播放信息中包含 Spider 站点的结果

- [ ] **Step 1: 修改播放相关函数**

在 `get_video_detail` 或播放相关函数中，添加 Spider 播放支持：

```rust
// 检查是否为 JS Spider 站点
if SpiderDispatcher::is_js_spider(3, &site.api) {
    let dispatcher = app_handle.state::<SpiderDispatcher>();
    let ext = String::new(); // TODO: 从配置中获取

    match dispatcher.player(3, &site.api, &ext, flag, id, &vip_flags).await {
        Ok(player_resp) => {
            // 根据 player_resp 构造播放信息
            if player_resp.parse == 0 && player_resp.url.is_some() {
                // 直接播放
                return Ok(player_resp.url.unwrap());
            }
            // 需要解析的情况
        }
        Err(e) => {
            tracing::warn!("Spider player failed: {}", e);
        }
    }
}
```

- [ ] **Step 2: 验证编译**

Run: `cargo check -p quantumtv`
Expected: 编译成功

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "feat: integrate Spider player into video playback flow"
```

---

### Task 9: 配置解析支持

**Files:**
- Modify: `src-tauri/src/commands/config.rs`

**Interfaces:**
- Consumes: SpiderDispatcher::is_js_spider
- Produces: 配置解析时正确识别 JS Spider 站点

- [ ] **Step 1: 在 config.rs 中添加导入**

```rust
use quantumtv_core::spider::dispatcher::SpiderDispatcher;
```

- [ ] **Step 2: 修改站点验证逻辑**

在 `validate_remote_url_against_config` 或相关验证逻辑中，跳过 JS Spider 站点的 URL 验证：

```rust
// 在验证前检查是否为 JS Spider
if SpiderDispatcher::is_js_spider(site_type, &api) {
    // JS Spider 的 api 不是 URL，跳过 URL 验证
    return Some(ApiSite { ... });
}
```

- [ ] **Step 3: 验证编译**

Run: `cargo check -p quantumtv`
Expected: 编译成功

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/config.rs
git commit -m "feat: support JS Spider sites in config parsing"
```

---

### Task 10: 单元测试

**Files:**
- Create: `crates/core/src/spider/quickjs_engine_test.rs`

**Interfaces:**
- Consumes: QuickJsEngine
- Produces: 测试用例

- [ ] **Step 1: 创建测试文件**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_js_spider() {
        assert!(SpiderDispatcher::is_js_spider(3, "https://example.com/spider.js"));
        assert!(SpiderDispatcher::is_js_spider(3, "https://example.com/spider.js?token=abc"));
        assert!(!SpiderDispatcher::is_js_spider(3, "csp_DoubanGuard"));
        assert!(!SpiderDispatcher::is_js_spider(1, "https://example.com/api.php/provide/vod"));
        assert!(!SpiderDispatcher::is_js_spider(3, "https://example.com/spider.py"));
    }

    #[test]
    fn test_fetch_sync() {
        let result = fetch_sync(
            "https://httpbin.org/get",
            "GET",
            &std::collections::HashMap::new(),
            None,
            10000,
        );
        assert!(result.is_ok());
        let resp = result.unwrap();
        assert_eq!(resp.status_code, 200);
    }
}
```

- [ ] **Step 2: 运行测试**

Run: `cargo test -p quantumtv-core -- spider`
Expected: 测试通过

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/spider/
git commit -m "test(spider): add unit tests for Spider module"
```

---

### Task 11: 端到端验证

**Files:**
- 无新文件，手动测试

**Interfaces:**
- Consumes: 所有前序任务
- Produces: 功能验证通过

- [ ] **Step 1: 构建项目**

Run: `cargo tauri dev`
Expected: 应用启动成功

- [ ] **Step 2: 测试搜索**

1. 进入设置 → 配置订阅
2. 输入一个包含 JS Spider 的 TVBox 订阅 URL
3. 保存配置
4. 进入搜索页面，输入关键词
5. 预期：搜索结果中包含 Spider 站点的结果

- [ ] **Step 3: 测试播放**

1. 点击搜索结果中的 Spider 站点视频
2. 选择集数
3. 预期：视频开始播放

- [ ] **Step 4: 最终 Commit**

```bash
git add -A
git commit -m "feat: complete TVBox Spider QuickJS support"
```

---

## 完成

所有任务完成后，TVBox Spider QuickJS 支持功能实现完毕。后续可以：
1. 添加更多宿主 API（如 proxy、cookie 支持）
2. 优化性能（如预加载 JS、并行执行）
3. 添加更多测试用例
