# QuantumTV Spider 搜索实现设计

日期: 2026-09-02

## 背景

用户配置了 `https://9280.kstore.space/wex.json` 订阅(全 `type: 3` Java spider 站点),搜索无结果。
根因:

1. `crates/api-server/src/tvbox.rs` 的 `search_handler` 是 stub,恒返回 `{ results: [] }`
2. 返回格式 `{ results }` 与调用方 `src-tauri/src/commands/video.rs` 期望的 `{ list }`(`ApiSearchResponse`)不匹配
3. wex.json 的 spider 字段是伪装成 `.jpg` 的 JAR URL(`URL;md5;<hash>`),且标准 `get_spider_jar` 下载的 CatVodSpider JAR 不含 `Wex*Guard` 类
4. 调用方对 spider 请求仅 6s 超时,JVM 启动 + 下载 JAR 会超时

## 方案

### 1. Java 运行时
- 安装 Eclipse Temurin JDK 17(winget),提供 `java` + `javac`

### 2. SpiderRunner.java(反射 runner,新增)
- 内嵌在 `crates/api-server/resources/SpiderRunner.java`,用 `include_str!` 打包
- 逻辑:URLClassLoader 加载 spider JAR → 反射实例化类 → 反射调 `init`(传 null Context 兜底,容忍不同签名)→ 调 `searchContent(query, quick)` → 用 JAR 自带 fastjson 序列化结果(无则手写反射 JSON)→ 输出 `{ "list": [...] }` 到 stdout
- 首次使用 `javac` 编译到 `.cache/runner/`,之后复用

### 3. 站点 spider JAR 下载 + 缓存
- 解析 `URL;md5;<hash>` 格式,取 URL 段下载,校验 MD5
- 缓存到 `.cache/sites/<site_key>/spider.jar` + `.cache/sites/<site_key>/spider.json`(元数据)
- 内存缓存 `moka`,TTL 24h

### 4. `search_handler` 完整实现
请求参数扩展:
- `site_key`、`query`(已有)
- `spider`(JAR URL,由 Tauri 端传入,避免 api-server 重新解析订阅)
- `class_name`(类名,由 Tauri 端从 api 去掉 `csp_` 前缀传入)

流程:
1. 用传入的 `spider` + `class_name`(缺省时从自身订阅配置按 `site_key` 查找)
2. 下载/取缓存 JAR,写临时文件
3. 子进程 `java -cp <jar>:<runner> <class> <query>`,15s 超时
4. 解析 stdout JSON,映射为 `ApiSearchItem`
5. 返回 `{ "list": [...] }`(对齐 `ApiSearchResponse`)

### 5. Tauri 端改动(video.rs)
- spider 请求 URL 增加 `spider`、`class_name` 参数
- 该请求超时从 6s 提到 15s

### 6. 响应格式对齐
- `SearchResponse` 改为 `{ list: [...] }`,字段映射 `ApiSearchItem`
  (vod_id / vod_name / vod_pic / vod_remarks / vod_year / vod_play_url 等)

## 测试

- 单元测试: `;md5;` URL 解析、`csp_` 类名提取、spider JSON → ApiSearchItem 映射、临时 JAR 写盘
- 冒烟: 启动 api-server,用 wex 一个 searchable 站点 curl `/api/search`
- 全量 `cargo test` + 前端 typecheck/build
