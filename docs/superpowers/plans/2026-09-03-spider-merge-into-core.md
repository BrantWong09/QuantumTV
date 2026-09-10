# Spider 执行合并到 crates/core,桌面端自包含

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 消除桌面端对独立 api-server(`127.0.0.1:3000`)的依赖——Spider 站点的搜索与详情直接在桌面端执行。api-server 保留为可选层,复用 core 的 Spider 能力。

**Background:** 桌面端 `search_with_cache_hit`(src-tauri/src/commands/video.rs)对 type:3 站点硬编码转发到 `http://127.0.0.1:3000/api/search`,而 api-server 是独立进程,多数用户不知道需要启动它,导致 Spider 站点搜索不可用。同时 `get_video_detail`/`get_video_detail_optimized` 对 Spider 站点仍走 CMS 的 `ac=videolist&ids=` 格式,必然失败——Spider 详情从未实现。

**Architecture:** 把 `crates/api-server/src/tvbox.rs` 中的 Spider 执行逻辑(SpiderRunner.java 编译、JAR 下载/缓存/MD5、Java 子进程搜索)抽到 `crates/core/src/spider/`,并提供 `spider_search` / `spider_detail` 公共 API。桌面端 `video.rs` 搜索与详情按 `site_type` 分流,Spider 站点直接调用 core 函数。api-server 的 HTTP handler 改为委托 core。

**Tech Stack:** Rust (Tauri 2, reqwest, tokio), Java subprocess bridge, TypeScript (React 19, Next.js 16)

## Global Constraints

- Rust edition: 2021
- `src-tauri` 不在 workspace,独立构建;`crates/core` 是共享库
- `crates/core` 需新增依赖 `md5`、`base64`(仅 core,不引入额外大依赖)
- 所有 Rust 必须过 `cargo check` + `cargo test`(workspace 与 src-tauri 各自)
- 所有 TypeScript 必须过 ESLint + typecheck
- 无 Java 时 Spider 站点**静默跳过**,不阻塞 CMS 搜索,不崩溃
- 保持 `SearchResult`(core)字段稳定,桌面端映射逻辑不破坏

---

### Task 1: core 新增 spider 模块骨架 + 资源文件

**Files:**
- Add: `crates/core/src/spider/mod.rs`
- Add: `crates/core/src/spider/SpiderRunner.java`
- Modify: `crates/core/Cargo.toml` (加 `md5`, `base64`)
- Modify: `crates/core/src/lib.rs` (声明 `pub mod spider`)

**Interfaces:**
- Consumes: 无
- Produces: `crates/core` 可编译的 spider 模块(暂空实现),SpiderRunner.java 进入 core

- [ ] **Step 1: 拷贝 SpiderRunner.java 到 core**

把 `crates/api-server/resources/SpiderRunner.java` 完整复制到 `crates/core/src/spider/SpiderRunner.java`。用 `include_str!("SpiderRunner.java")` 内嵌,路径相对 `mod.rs`。

- [ ] **Step 2: 新增依赖**

`crates/core/Cargo.toml` 增加:
```toml
md5 = "0.7"
base64 = "0.22"
```

- [ ] **Step 3: 创建空模块**

`crates/core/src/spider/mod.rs` 占位,`lib.rs` 加 `pub mod spider;`。

- [ ] **Step 4: 验证编译**

Run: `cargo check --manifest-path crates/core/Cargo.toml`
Expected: no errors

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/spider crates/core/Cargo.toml crates/core/src/lib.rs Cargo.lock
git commit -m "feat(spider): scaffold spider module in core"
```

---

### Task 2: 移植 JAR 下载/缓存/MD5 逻辑到 core

**Files:**
- Modify: `crates/core/src/spider/mod.rs`
- (参考) `crates/api-server/src/tvbox.rs:105-230, 1051-1165`

**Interfaces:**
- Consumes: spider spec 字符串(`URL;md5;<hash>`)
- Produces: `parse_spider_spec`, `calculate_md5`, `fetch_remote`, `get_site_spider_jar` 私有函数 + 对外 `SpiderJarHandle`

- [ ] **Step 1: 写单元测试(先红)**

在 `mod.rs` 加 `#[cfg(test)]` 测试:
- `parse_spider_spec("URL;md5;<hash>") -> (url, Some(hash))`
- `parse_spider_spec("URL") -> (url, None)`
- `calculate_md5` 已知向量

- [ ] **Step 2: 移植实现**

从 `tvbox.rs` 搬运:
- `parse_spider_spec`
- `calculate_md5`
- `fetch_remote`(reqwest,校验 ZIP 头 `PK`)
- `get_site_spider_jar`(磁盘缓存按 md5 或 url 哈希命名,MD5 校验)
- 缓存目录默认 `PathBuf::from(".cache")`,用参数注入以便桌面端可覆盖(桌面端应写入 app data 目录)

对外暴露:
```rust
pub struct SpiderJarHandle { pub path: PathBuf }
pub async fn ensure_site_spider_jar(site_key: &str, spec: &str, cache_root: &Path) -> Result<SpiderJarHandle, String>
```

- [ ] **Step 3: 跑测试**

Run: `cargo test -p quantumtv-core spider`
Expected: 全绿

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/spider/mod.rs
git commit -m "feat(spider): port jar download/cache/md5 into core"
```

---

### Task 3: 移植 Runner 编译 + Java 子进程搜索到 core

**Files:**
- Modify: `crates/core/src/spider/mod.rs`
- (参考) `crates/api-server/src/tvbox.rs:1097-1237`

**Interfaces:**
- Consumes: `SpiderJarHandle`, class_name, query
- Produces: `spider_search(site_key, query, class_name, spider_spec, cache_root) -> Result<Vec<SpiderSearchItem>, String>`

- [ ] **Step 1: 移植辅助函数**

- `check_java_available() -> bool`(`java -version` 探测)
- `ensure_runner_compiled(cache_root) -> Result<PathBuf, String>`(javac 编译 `SpiderRunner.java`,复用 `.class`)
- `run_java(classpath, args...) -> Result<String, String>`(tokio 子进程,`-Dfile.encoding=UTF-8`)

- [ ] **Step 2: 定义对外结果类型**

```rust
#[derive(Serialize, Deserialize)]
pub struct SpiderSearchItem {
    pub vod_id: serde_json::Value,
    pub vod_name: String,
    pub vod_pic: String,
    #[serde(default)] pub vod_remarks: Option<String>,
    #[serde(default)] pub vod_play_url: Option<String>,
    #[serde(default)] pub vod_year: Option<String>,
    #[serde(default)] pub vod_content: Option<String>,
    #[serde(default)] pub vod_class: Option<String>,
    #[serde(default)] pub vod_douban_id: Option<serde_json::Value>,
    #[serde(default)] pub type_name: Option<String>,
}
```
(与 api-server 现 `SearchResultItem` 同构,序列化 `{ "list": [...] }`)

- [ ] **Step 3: 实现 `spider_search`**

流程:解析 spec → ensure jar → ensure runner → 子进程 `java -cp <jar>;<runner> SpiderRunner search <class> <query>`(20s 超时)→ 解析 `{list}` → 返回 items。Java 不可用返回 `Err("Java 运行时不可用")`。

- [ ] **Step 4: 单元测试**

- `spider_search` 在无 Java 环境返回 `Err`(不 panic)
- 空 spec 返回 `Err`
- runner 编译缓存逻辑

Run: `cargo test -p quantumtv-core spider`

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/spider/mod.rs
git commit -m "feat(spider): port java search runner into core"
```

---

### Task 4: 增强 SpiderRunner.java 支持详情(detailContent)

**Files:**
- Modify: `crates/core/src/spider/SpiderRunner.java`

**Interfaces:**
- Consumes: 命令行参数 `search` / `detail`
- Produces: 统一输出 `{ "list": [...] }`(搜索与详情一致)

- [ ] **Step 1: 重构 main 参数解析**

`SpiderRunner <action> <className> <arg>`:
- `search <className> <query>` → `searchContent(query[, quick])`
- `detail <className> <id>` → `detailContent(new String[]{id})`(兼容 `String` / `String[]` 签名,兜底容忍)

- [ ] **Step 2: 新增 `doDetail` 反射调用**

复用 `findMethod` + `tryInit` + `toJson`(reflection 序列化已具备)。对 `detailContent` 返回:
- 若是 `String` 已含 JSON → 直接输出
- 若是对象/集合 → `toJson`

- [ ] **Step 3: 同步 api-server 资源文件**

保持 `crates/api-server/resources/SpiderRunner.java` 与新文件一致(或改为引用 core 的副本,见 Task 7)。

- [ ] **Step 4: 验证**

本地 `javac -encoding UTF-8 SpiderRunner.java` 编译无错(若本机有 JDK)。

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/spider/SpiderRunner.java crates/api-server/resources/SpiderRunner.java
git commit -m "feat(spider): add detailContent support to SpiderRunner"
```

---

### Task 5: core 新增 `spider_detail` API

**Files:**
- Modify: `crates/core/src/spider/mod.rs`

**Interfaces:**
- Consumes: `SpiderJarHandle`, class_name, video_id
- Produces: `spider_detail(site_key, video_id, class_name, spider_spec, cache_root) -> Result<SpiderSearchItem, String>`

- [ ] **Step 1: 实现**

与 `spider_search` 共用 JAR/runner 逻辑,仅子进程命令为 `SpiderRunner detail <class> <id>`,解析 `{list}` 取第一条。Java 不可用返回 `Err`。

- [ ] **Step 2: 单元测试**

- 无 Java 返回 `Err` 不 panic
- 空 spec / 空 id 返回 `Err`

Run: `cargo test -p quantumtv-core spider`

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/spider/mod.rs
git commit -m "feat(spider): add spider_detail API to core"
```

---

### Task 6: 桌面端搜索改为直接调用 core

**Files:**
- Modify: `src-tauri/src/commands/video.rs` (search_with_cache_hit 内 type:3 分支)

**Interfaces:**
- Consumes: core `spider_search`
- Produces: 桌面端 Spider 搜索不再依赖 `127.0.0.1:3000`

- [ ] **Step 1: 替换 HTTP 转发**

`video.rs:1187-1219` 的 type:3 分支:由拼接 URL + `client.get()` 改为直接调用:
```rust
let items = quantumtv_core::spider::spider_search(
    &site_clone.key, &query, class_name, &spider, &cache_root
).await;
```
`cache_root` 用 app data 目录(`StorageManager` 可提供)。失败/Java 不可用时:流式事件仍照常发射(空结果),不抛错。

- [ ] **Step 2: 保持结果映射**

把 `Vec<SpiderSearchItem>` 映射为 `SearchResult`(复用现有 1280-1309 行映射逻辑,提取成辅助函数)。

- [ ] **Step 3: 并发与超时**

保留 `Semaphore(20)` 与每站点独立超时(建议 `spider_search` 内部 20s 超时;外层 tokio timeout 25s 兜底)。

- [ ] **Step 4: 编译 + 测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: no errors

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "feat(search): run spider search directly in desktop, drop api-server dependency"
```

---

### Task 7: 桌面端详情按 site_type 分流

**Files:**
- Modify: `src-tauri/src/commands/video.rs` (get_video_detail, get_video_detail_optimized)

**Interfaces:**
- Consumes: core `spider_detail`
- Produces: Spider 站点详情走 Java 执行,不再走 CMS `ac=videolist&ids=`

- [ ] **Step 1: 两个详情命令加 site_type 判断**

解析 `resolve_enabled_source` 得到的 `ApiSite.site_type`:
- `Some(3)` → `quantumtv_core::spider::spider_detail(source, id, class_name, spider, cache_root)`
- 否则 → 现有 CMS 路径

class_name 取 `site.api` 去 `csp_` 前缀;spider 取 `site.spider`。

- [ ] **Step 2: 复用结果映射**

`SpiderSearchItem -> SearchResult` 与 Task 6 共用辅助函数,含 `parse_episodes`。

- [ ] **Step 3: 编译 + 测试**

Run: `cargo check --manifest-path src-tauri/Cargo.toml`
Run: `cargo test --manifest-path src-tauri/Cargo.toml`

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/commands/video.rs
git commit -m "feat(detail): route spider site detail through core java execution"
```

---

### Task 8: api-server 改为委托 core(可选层瘦身)

**Files:**
- Modify: `crates/api-server/src/tvbox.rs` (search_handler 委托 core)
- Modify: `crates/api-server/Cargo.toml` (依赖 base64/md5 若不再直接使用可移除)
- (可选) Delete: `crates/api-server/resources/SpiderRunner.java`(改用 core 副本)

**Interfaces:**
- Consumes: core `spider_search`
- Produces: `/api/search` 端点保持行为不变,但实现来自 core

- [ ] **Step 1: search_handler 改调 core**

`tvbox.rs:1252` 的 `search_handler` 去掉本地 JAR/runner/Java 逻辑,改为:
```rust
quantumtv_core::spider::spider_search(site_key, query, class_name, spider_spec, cache_root)
```
并转成 `SearchResponse { list }`。缓存目录沿用 `.cache`。

- [ ] **Step 2: 移除 api-server 内重复实现**

删除 `tvbox.rs` 中的 `parse_spider_spec`/`get_site_spider_jar`/`ensure_runner_compiled`/`run_java_search`/`check_java_available`/`SEARCH_CACHE_DIR` 等(仅在 api-server 内部使用的),`SpiderRunner.java` 改为 `include_str!` 引用 core 的副本或删除。

- [ ] **Step 3: 编译 + 测试**

Run: `cargo check --manifest-path crates/api-server/Cargo.toml`
Run: `cargo test -p quantumtv-api`

- [ ] **Step 4: Commit**

```bash
git add crates/api-server/src/tvbox.rs crates/api-server/Cargo.toml crates/api-server/resources
git commit -m "refactor(api): delegate spider search to core"
```

---

### Task 9: 端到端验证 + 文档

**Files:**
- 验证: 手动
- 文档: `README.md`(若涉及描述变化),不主动新增文档文件

**Interfaces:**
- Consumes: 所有前序 Task
- Produces: 桌面端不依赖 api-server 即完成 Spider 搜索与详情

- [ ] **Step 1: 无 Java 环境冒烟**

桌面端搜索含 type:3 站点 → 不崩溃,CMS 结果正常,Spider 结果为空;详情页对 Spider 源提示"需要 Java"。

- [ ] **Step 2: 有 Java 环境冒烟**

(本机 `java -version` OK 时)桌面端搜索 wex 订阅下 type:3 站点返回结果;点击进入详情页选集正常;播放链路不受影响。

- [ ] **Step 3: 全量检查**

Run: `cargo test` (workspace)
Run: `npm run typecheck`
Run: `npm run lint`(若库提供)

- [ ] **Step 4: 最终 Commit**

```bash
git add -A
git commit -m "feat: desktop self-contained spider search & detail, api-server optional"
```
