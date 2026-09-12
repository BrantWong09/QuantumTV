# QuantumTV Bridge 与播放性能优化实施方案

> 目标：解决当前 QuantumTV 在 Android Bridge 场景下的两个核心性能问题：
>
> 1. 搜索过程中 Bridge 性能差，大量请求导致页面卡顿甚至无响应。
> 2. 播放返回阶段重复触发搜索/解析，导致播放启动慢、桥接器负载高。
>
> 本方案基于当前 main 分支架构设计，不推翻现有 V2 架构。

---

# 1. 当前问题

当前 QuantumTV 已经完成主要架构重构：

```text
Source
   ↓
Resolver
   ↓
MediaResource
   ↓
Playback Gateway
   ↓
PlaybackManager
   ↓
mpv
```

Android Bridge 作为特殊 Resolver 能力存在：

```text
Desktop
   ↓
Virtual Bridge
   ↓
TCP Tunnel
   ↓
Android Spider
   ↓
HTTP Response
```

目前 Bridge 的数据流更接近：

```text
HTTP Request
    ↓
创建本地 TCP Client
    ↓
读取完整 HTTP Request
    ↓
封装 Frame
    ↓
TCP Tunnel
    ↓
Android Spider
    ↓
完整 HTTP Response
    ↓
Frame
    ↓
oneshot
    ↓
HTTP Response
    ↓
关闭连接
```

这种模型适合：

```text
搜索
详情
单次解析
```

但不适合：

```text
高频搜索
连续详情请求
播放解析
播放过程中的重试
大量并发请求
```

因此必须把 Bridge 从：

> “一次 HTTP 请求经过一次隧道”

升级为：

> “长期存在的 Bridge Session + 请求复用 + 缓存 + 请求去重”。

---

# 2. 总体目标

本次优化分为两个方向。

## A. Bridge 数据面优化

目标：

```text
搜索 10 个请求
↓
不应该产生 10 次 Bridge 建连

而应该：

1 个长期 Bridge Session
    ↓
多个并发 request
    ↓
Frame multiplex
    ↓
Android Spider
```

同时增加：

- 请求去重
- 短期缓存
- 并发限制
- 超时
- 优先级
- 请求取消
- Bridge health state
- 搜索/播放隔离

---

# 3. Bridge 新架构

改造后的架构：

```text
                         ┌──────────────────────┐
                         │      BridgeSession   │
                         │                      │
                         │ connection           │
                         │ request id           │
                         │ pending requests     │
                         │ concurrency limit    │
                         │ health               │
                         └──────────┬───────────┘
                                    │
             ┌──────────────────────┼──────────────────────┐
             │                      │                      │
          Search                  Detail                Resolve
             │                      │                      │
             └──────────────────────┼──────────────────────┘
                                    │
                             Multiplex Frame
                                    │
                              TCP Tunnel
                                    │
                              Android APK
```

核心原则：

> Tunnel 是长期连接，HTTP Request 是逻辑请求。

不要再把：

```text
TCP Connection
=
HTTP Request
```

混在一起。

应该变成：

```text
TCP Connection
=
Bridge Session

HTTP Request
=
Frame
```

---

# 4. BridgeSession

新增：

```text
crates/core/src/bridge/session.rs
```

建议结构：

```rust
pub struct BridgeSession {
    pub device: String,

    writer: mpsc::Sender<BridgeRequest>,

    pending:
        DashMap<u32, oneshot::Sender<BridgeResponse>>,

    next_id:
        AtomicU32,

    health:
        AtomicU8,

    inflight:
        AtomicU32,

    semaphore:
        Arc<Semaphore>,
}
```

如果暂时不想增加 `DashMap`，可以继续使用：

```rust
Arc<Mutex<HashMap<...>>>
```

但不要让全局 Mutex 成为高并发路径上的瓶颈。

---

# 5. Request 生命周期

新的请求流程：

```text
bridge_request()
      │
      ▼
生成 request_id
      │
      ▼
获取 semaphore permit
      │
      ▼
注册 pending[id]
      │
      ▼
writer.send(frame)
      │
      ▼
等待 oneshot
      │
      ├── success
      │
      ├── timeout
      │
      └── bridge disconnected
      │
      ▼
remove pending[id]
      │
      ▼
释放 semaphore
```

要求：

```text
任何异常都必须 remove pending
```

否则会产生：

```text
PENDING 泄漏
```

最终导致：

```text
桥接越来越慢
内存增长
请求 ID 堆积
```

---

# 6. Bridge 并发控制

不要允许无限并发请求。

增加：

```rust
const MAX_BRIDGE_INFLIGHT: usize = 4;
```

第一版建议：

```text
搜索：最多 4
详情：最多 2
播放解析：最多 1
```

更推荐使用优先级队列：

```text
Priority 0
Playback Resolve

Priority 1
Detail

Priority 2
Search

Priority 3
Background
```

原因：

如果用户正在播放：

```text
播放解析请求
```

必须优先于：

```text
搜索更多结果
```

否则搜索会把 Bridge 的并发额度全部吃掉。

---

# 7. 搜索请求必须支持取消

当前用户输入：

```text
斗破
```

随后输入：

```text
斗破苍穹
```

不应该继续执行：

```text
斗破
```

对应的 Bridge 请求。

正确流程：

```text
用户输入：
斗
   ↓
斗破
   ↓
斗破苍穹
```

最终：

```text
斗
    cancel

斗破
    cancel

斗破苍穹
    execute
```

前端每次搜索生成：

```text
search_generation
```

例如：

```text
generation = 103
```

Rust 返回结果时必须携带：

```json
{
  "generation": 103,
  "source": "...",
  "results": [...]
}
```

前端只接受：

```text
generation == currentGeneration
```

旧请求直接丢弃。

---

# 8. 搜索结果缓存

Bridge 搜索必须增加短期缓存。

建议：

```text
Key:

bridge-search:
    spider_id
    keyword
    page
    filter
```

Value：

```text
SearchResult
```

TTL：

```text
30 ~ 120 秒
```

推荐第一版：

```text
60 秒
```

原因：

用户经常会出现：

```text
搜索
→ 点详情
→ 返回
→ 再次搜索
```

如果每次都重新调用 Android Spider：

```text
Bridge
→ Android
→ Spider
→ Network
```

这是纯浪费。

---

# 9. Detail 缓存

详情缓存建议：

```text
TTL = 5 ~ 10 分钟
```

Key：

```text
source_id + vod_id
```

因为详情变化频率远低于搜索。

---

# 10. 最重要：播放解析禁止重新 Search

这是本次优化的核心。

当前必须避免：

```text
Search
 ↓
Result
 ↓
Detail
 ↓
Play
 ↓
再次 Search
 ↓
再次 Detail
 ↓
再次 Resolve
```

正确流程：

```text
Search
 ↓
SearchResult
 ↓
用户点击
 ↓
Detail
 ↓
Episode
 ↓
Resolve
 ↓
MediaResource
 ↓
Playback
```

播放解析只允许使用：

```text
vod_id
episode_id
source_id
existing metadata
```

不得重新执行：

```text
search(keyword)
```

---

# 11. 建立 PlaybackResolveContext

新增：

```text
PlaybackResolveContext
```

例如：

```rust
pub struct PlaybackResolveContext {
    pub source_id: String,
    pub vod_id: String,
    pub episode_id: String,

    pub title: Option<String>,
    pub play_url: Option<String>,

    pub spider_source: Option<String>,

    pub bridge_device: Option<String>,
}
```

播放时：

```text
Play Episode
      │
      ▼
PlaybackResolveContext
      │
      ▼
Resolver
      │
      ▼
MediaResource
```

而不是：

```text
Play Episode
      │
      ▼
Search
      │
      ▼
Detail
      │
      ▼
Resolver
```

---

# 12. Resolve Cache

播放解析必须增加缓存。

Key：

```text
resolve:
    source_id
    vod_id
    episode_id
```

Value：

```text
MediaResource
```

TTL 需要根据 URL 类型决定。

## 普通 URL

```text
5 ~ 30 分钟
```

## 临时签名 URL

```text
不要直接长期缓存
```

而是缓存：

```text
ResolverResult
```

而不是：

```text
最终 URL
```

例如：

```text
ResolverResult
{
    source,
    vod_id,
    episode_id,
    headers,
    cookie,
    url,
    expires_at
}
```

如果：

```text
expires_at - now < 60s
```

重新 resolve。

---

# 13. 防止重复 Resolve

这是第二个非常重要的优化。

假设同时出现：

```text
mpv 初始化
播放器状态检查
UI refresh
播放组件 mount
```

可能产生：

```text
Resolve(A)
Resolve(A)
Resolve(A)
Resolve(A)
```

必须做：

```text
SingleFlight
```

效果：

```text
Resolve(A)
   │
   ├── caller 1
   ├── caller 2
   ├── caller 3
   └── caller 4
        ↓
    实际只执行一次
```

新增：

```rust
ResolveFlightKey {
    source_id,
    vod_id,
    episode_id,
}
```

同一个 Key 在执行期间：

```text
共享 Future / oneshot
```

---

# 14. Bridge Search Cache + Resolve SingleFlight

最终：

```text
Search
 │
 ├── Cache Hit → 直接返回
 │
 └── Cache Miss
        │
        ▼
   Bridge Session
        │
        ▼
   Android Spider
```

播放：

```text
Play
 │
 ▼
Resolve Key
 │
 ├── Cache Hit
 │
 ├── Flight Existing
 │       ↓
 │    await existing
 │
 └── New Flight
         ↓
       Bridge
         ↓
       Resolve
```

---

# 15. Bridge HTTP 层必须连接复用

当前 `handle_bridge_client()`：

```text
read_http_request()
      ↓
send frame
      ↓
await response
      ↓
write response
      ↓
shutdown
```

其中：

```text
Connection: close
```

是明显的性能损耗。

必须修改为：

```text
Connection: keep-alive
```

同时允许：

```text
一个 TCP Client
    ↓
多个 HTTP Request
```

---

# 16. Virtual Bridge HTTP Session

改造：

```text
handle_bridge_client()
```

为：

```text
handle_bridge_session()
```

流程：

```text
accept TCP
    ↓
loop
    ↓
read HTTP request
    ↓
send frame
    ↓
await response
    ↓
write HTTP response
    ↓
if keep-alive
    └── continue
else
    └── close
```

第一阶段可以：

```text
HTTP/1.1 keep-alive
```

不需要马上实现 HTTP/2。

---

# 17. Bridge Frame 不应该复制过多数据

当前：

```rust
encode_frame(id, payload)
```

会创建：

```text
8 + payload.len()
```

的新 Vec。

搜索结果问题不大。

但如果以后 Bridge 返回大量 JSON：

```text
几十 KB
几百 KB
```

会产生不必要的复制。

第一阶段先不做复杂 zero-copy。

但必须：

```text
避免 payload 多次 clone
```

建议：

```rust
mpsc::Sender<BridgeFrame>
```

而不是：

```rust
mpsc::Sender<(u32, Vec<u8>)>
```

统一 Frame 生命周期。

---

# 18. Bridge Request Timeout

必须给不同请求设置不同 Timeout。

建议：

```text
Search:
    8s

Detail:
    8s

Resolve:
    15s

Health:
    2s
```

不要统一使用超长 timeout。

尤其不能：

```text
30s
60s
120s
```

否则 Bridge 卡住以后：

```text
4 个请求
× 30s
```

很快就会把整个请求队列拖死。

---

# 19. Bridge 熔断

增加：

```text
BridgeHealth
```

状态：

```text
Healthy
Degraded
Unavailable
Recovering
```

例如：

```text
连续 3 次 timeout
       ↓
Degraded

连续 5 次失败
       ↓
Unavailable
```

Unavailable 状态下：

```text
Search
```

不要立即疯狂重试。

等待：

```text
1s
2s
5s
10s
```

再恢复。

---

# 20. Search 与 Playback 使用不同队列

非常重要。

不要：

```text
所有 Bridge 请求
        ↓
一个 queue
```

建议：

```text
Bridge
│
├── Playback Queue
│
│   ├── Resolve
│   └── Detail
│
└── Search Queue
    ├── Search
    └── Background
```

播放优先级永远高于搜索。

---

# 21. 搜索期间播放不能被阻塞

目标：

```text
用户正在搜索
        ↓
用户点击播放
        ↓
播放 Resolve
        ↓
立即获得 Bridge 优先权
```

而不是：

```text
搜索还有 8 个请求
        ↓
播放请求排队
        ↓
用户等待
```

---

# 22. Android Bridge 端也需要并发控制

Desktop：

```text
MAX_INFLIGHT = 4
```

Android：

```text
SpiderExecutor
```

建议：

```text
搜索：
最多 2

详情：
最多 1

播放解析：
最多 1
```

不要让 Android Spider 同时跑大量任务。

---

# 23. Android Spider 搜索结果缓存

如果 Android APK 本身可以控制，增加：

```text
ConcurrentHashMap<SearchKey, CacheEntry>
```

TTL：

```text
60 秒
```

这样：

```text
Desktop Cache
+
Bridge Cache
+
Android Cache
```

形成三级缓存。

不过优先级应该是：

```text
Desktop Cache
    ↓
Bridge
    ↓
Android Cache
    ↓
Spider
```

不要一开始就在三个地方实现复杂缓存。

第一阶段只做 Desktop Cache。

---

# 24. 播放链路重新定义

最终播放链路应该是：

```text
用户点击 Episode
       │
       ▼
PlaybackManager
       │
       ▼
ResolveCache
       │
       ├── HIT ─────────────┐
       │                    │
       └── MISS             │
            │               │
            ▼               │
        Resolver            │
            │               │
            ▼               │
        Bridge              │
            │               │
            ▼               │
        MediaResource       │
            │               │
            └───────────────┘
                    │
                    ▼
              PlaybackGateway
                    │
                    ▼
                   mpv
```

注意：

```text
PlaybackManager
```

不允许：

```text
Search
```

---

# 25. Netdisk Gateway 优化

当前网盘播放：

```text
mpv
 ↓
127.0.0.1 proxy
 ↓
netdisk
```

这是正确方向。

但必须确保：

```text
proxy
```

不是：

```text
一次播放请求
    ↓
重新调用 Spider
```

而应该：

```text
MediaResource
    ↓
Gateway
    ↓
直接请求实际媒体 URL
```

---

# 26. Netdisk Proxy 必须完全流式

保持：

```text
Range
Content-Range
Content-Length
Accept-Ranges
```

透传。

禁止：

```text
read_to_end()
```

禁止：

```text
完整文件进入内存
```

禁止：

```text
完整文件落盘
```

正确：

```text
Upstream
   ↓
stream chunks
   ↓
mpv
```

---

# 27. Range 请求必须直接透传

例如 mpv：

```http
Range: bytes=50000000-
```

Gateway：

```http
Range: bytes=50000000-
```

不要：

```text
Desktop
 ↓
重新请求 0-
 ↓
自己 seek
```

否则拖动会非常慢。

---

# 28. Connection Pool

Gateway 使用：

```text
reqwest Client
```

必须全局复用。

不要：

```rust
Client::new()
```

在每个播放请求里面创建。

应该：

```text
GatewayState
    │
    └── reqwest::Client
```

长期存在。

---

# 29. HTTP Client 参数

建议：

```text
pool_idle_timeout = 90s
connect_timeout   = 5s
tcp_keepalive     = 30s
```

播放请求：

```text
read timeout
```

不要设置过短。

因为视频长连接可能持续数小时。

---

# 30. Buffer 策略

Gateway 不需要主动做巨大 buffer。

建议：

```text
8KB ~ 64KB
```

即可。

不要为了所谓“高速播放”设置：

```text
4MB
8MB
16MB
```

这通常只是增加内存，并不会解决源站带宽问题。

---

# 31. 播放启动优化

目标：

```text
点击播放
        ↓
< 500ms
开始启动播放流程
```

这里的关键不是强行提高网络速度，而是减少：

```text
Search
Detail
Resolve
Probe
Search
Resolve
```

这些串行操作。

理想：

```text
Episode 已经拥有播放信息
        ↓
直接 Resolve
        ↓
MediaResource
        ↓
mpv
```

---

# 32. Episode 数据模型增强

Search/Detail 返回 Episode 时，应尽量携带：

```rust
Episode {
    id,
    name,
    index,

    play_url,
    source_id,
    vod_id,

    resolver_hint,
}
```

这样：

```text
播放
```

不需要再次猜测：

```text
source
vod
episode
```

---

# 33. Resolver Hint

可以增加：

```rust
ResolverHint {
    resolver_type,
    source_id,
    requires_bridge,
}
```

例如：

```json
{
  "resolver_type": "wex",
  "source_id": "xxx",
  "requires_bridge": true
}
```

播放时直接：

```text
Resolver
 ↓
WexResolver
 ↓
Bridge
```

不要：

```text
ResolverManager
 ↓
重新遍历所有 Resolver
```

---

# 34. Resolver Selection Cache

增加：

```text
source_id → resolver
```

缓存。

不要每次：

```text
遍历所有 Resolver
```

例如：

```text
source A
 → WexResolver

source B
 → CmsResolver

source C
 → SpiderResolver
```

Resolver Manager 可以直接路由。

---

# 35. 不要在 UI 中轮询 Bridge

当前 Bridge status 已经存在：

```text
get_bridge_status()
```

如果前端高频：

```text
setInterval(...)
```

轮询：

```text
100ms
200ms
```

需要停止。

推荐：

```text
500ms ~ 1s
```

或者改成事件通知。

Bridge 状态只有：

```text
idle
starting
ready
failed
```

没必要每秒几十次 IPC。

---

# 36. Bridge Metrics

必须增加性能指标。

每个请求记录：

```text
request_id
request_type
source_id
start_time
queue_time
send_time
bridge_time
response_time
total_time
payload_size
success
error
```

日志：

```text
[BridgePerf]
type=resolve
source=wex
queue=12ms
bridge=820ms
total=841ms
size=18KB
```

搜索：

```text
[BridgePerf]
type=search
source=wex
queue=0ms
bridge=430ms
total=435ms
```

---

# 37. 关键指标

重点观察：

```text
P50
P90
P95
P99
```

尤其：

```text
Search total
Resolve total
Bridge queue time
Bridge execution time
Playback startup time
```

---

# 38. 性能验收标准

## 搜索

10 个来源：

```text
首批结果 < 1s
```

Bridge Source：

```text
单次请求 P95 < 3s
```

具体取决于 Android Spider 和源站速度。

---

## 重复搜索

第一次：

```text
1000ms
```

第二次：

```text
< 100ms
```

缓存命中情况下不允许再次访问 Bridge。

---

## 播放

从：

```text
点击 Episode
```

到：

```text
mpv 开始打开资源
```

目标：

```text
< 1s
```

如果源站本身慢，不要求网络层奇迹，但：

```text
QuantumTV 自己不能重复 Search / Detail / Resolve。
```

---

# 39. 第一阶段实施顺序

不要一次全部改完。

按照以下顺序。

## Phase 1

Bridge Request Manager

实现：

```text
BridgeSession
Request ID
pending
timeout
concurrency
cancel
```

---

## Phase 2

Bridge HTTP Keep-Alive

修改：

```text
handle_bridge_client
```

支持：

```text
多个 HTTP request / TCP connection
```

---

## Phase 3

Search Cache

实现：

```text
SearchCache
TTL 60s
```

---

## Phase 4

Resolve Cache

实现：

```text
ResolveCache
```

---

## Phase 5

Resolve SingleFlight

确保：

```text
同一 Episode
同时只允许一个 Resolver 执行。
```

---

## Phase 6

Playback Priority

实现：

```text
Playback > Detail > Search
```

---

## Phase 7

Netdisk Gateway

确认：

```text
Range
Keep-Alive
Connection Pool
Streaming
```

全部正确。

---

# 40. 推荐修改文件

重点检查：

```text
crates/core/src/bridge/tunnel.rs
crates/core/src/bridge/mod.rs
crates/core/src/netdisk_proxy.rs
crates/core/src/resolver.rs
crates/core/src/search_aggregation.rs

crates/core/src/playback/manager.rs
crates/core/src/playback/state.rs

src-tauri/src/commands/search.rs
src-tauri/src/commands/playback.rs
src-tauri/src/commands/bridge.rs
```

同时检查：

```text
src/app/play/page.tsx
```

重点搜索：

```text
search(
resolve(
playerContent(
detail(
invoke(
```

寻找播放过程中重新触发 Search 的调用链。

---

# 41. 禁止的修改方式

AI Coding Agent 不允许：

```text
❌ 简单增加 timeout

❌ 简单增加线程

❌ 无限提高 Bridge 并发

❌ 每次播放重新初始化 Bridge

❌ 播放失败就重新 Search

❌ 在 UI 中不断 retry

❌ 用 setInterval 高频刷新播放状态

❌ 把整个视频读进内存

❌ 通过增加 buffer 掩盖网络问题

❌ 为了解决卡顿直接增加更多 Tokio task
```

这些方式可能让：

```text
短期看起来快一点
```

但最终会把：

```text
CPU
内存
Bridge
Android Spider
```

全部拖死。

---

# 42. 最终目标架构

最终 QuantumTV 应该形成：

```text
                         QuantumTV
                              │
              ┌───────────────┴───────────────┐
              │                               │
          Search Plane                   Playback Plane
              │                               │
        Search Manager                 PlaybackManager
              │                               │
          Search Cache                  Resolve Cache
              │                               │
          Search Queue                  SingleFlight
              │                               │
              └──────────────┬────────────────┘
                             │
                       Resolver Manager
                             │
                ┌────────────┼────────────┐
                │            │            │
              CMS         Spider       Netdisk
                             │
                           Bridge
                             │
                       Bridge Session
                             │
                     Multiplexed Tunnel
                             │
                        Android APK
```

播放：

```text
Episode
 ↓
ResolveCache
 ↓
SingleFlight
 ↓
Resolver
 ↓
Bridge
 ↓
MediaResource
 ↓
Gateway
 ↓
mpv
```

搜索：

```text
Keyword
 ↓
SearchCache
 ↓
SearchQueue
 ↓
BridgeSession
 ↓
Android Spider
 ↓
Result
```

两个平面互相隔离。

---

# 43. 最重要的三个架构原则

以后 QuantumTV 的代码必须遵守这三个原则。

## 原则 1：Bridge 是 Session，不是 Request

错误：

```text
request → connect → request → close
```

正确：

```text
connect
   ↓
BridgeSession
   ↓
request
request
request
request
   ↓
close
```

---

## 原则 2：播放是 Resolve，不是 Search

错误：

```text
Play
 ↓
Search
 ↓
Detail
 ↓
Resolve
```

正确：

```text
Play
 ↓
Episode Context
 ↓
Resolve
```

---

## 原则 3：同一个资源不能同时 Resolve N 次

错误：

```text
Resolve(A)
Resolve(A)
Resolve(A)
Resolve(A)
```

正确：

```text
          ┌── caller 1
          ├── caller 2
Resolve(A)├── caller 3
          └── caller 4
```

最终只执行：

```text
1 次 Resolve
```

---

# 44. AI Coding Agent 执行要求

开始修改之前必须：

1. 阅读现有：
   - `docs/architecture/README.md`
   - `00-current-state.md`
   - `02-resolver.md`
   - `03-playback.md`
   - `04-gateway.md`
   - `05-ipc.md`

2. 阅读：
   - `crates/core/src/bridge/tunnel.rs`
   - `crates/core/src/resolver.rs`
   - `crates/core/src/search_aggregation.rs`
   - `crates/core/src/netdisk_proxy.rs`
   - `crates/core/src/playback/manager.rs`

3. 先画出当前真实调用链。

4. 找出：
   ```text
   Search → Detail → Resolve → Bridge
   ```
   的全部调用路径。

5. 特别检查：
   ```text
   Play → Search
   ```
   是否存在。

6. 不允许在没有找到调用链之前直接重构。

---

# 45. 最终验收

必须通过：

```bash
npm run lint
npm run typecheck
npm test

cd src-tauri
cargo test
```

增加至少以下测试：

```text
BridgeSessionTest

SearchCacheTest

ResolveCacheTest

ResolveSingleFlightTest

BridgeTimeoutTest

BridgeCancellationTest

PlaybackPriorityTest

NetdiskRangeTest
```

重点测试：

```text
同一个 Episode 并发播放 5 次

预期：

Bridge Resolve = 1 次
```

以及：

```text
搜索进行过程中点击播放

预期：

Playback Resolve 不需要等待 Search 完成
```

以及：

```text
搜索同一个关键词两次

预期：

第二次直接命中 Cache
```

以及：

```text
Bridge 断开

预期：

所有 pending request 在有限时间内失败
不能永久挂起。
```

---

# 46. 实施优先级

最终优先级：

```text
P0
★★★★★
禁止播放重复 Search / Resolve

P0
★★★★★
Bridge Session + Request Multiplex

P0
★★★★★
Resolve SingleFlight

P1
★★★★☆
Search Cache

P1
★★★★☆
Resolve Cache

P1
★★★★☆
Playback Priority

P1
★★★★☆
HTTP Keep-Alive

P2
★★★☆☆
Bridge Metrics

P2
★★★☆☆
Circuit Breaker

P2
★★★☆☆
Android Cache

P3
★★☆☆☆
Zero-copy / 更深层协议优化
```

不要一上来做 P3。

当前真正的性能问题不是：

```text
Vec clone
```

而是：

```text
重复请求
请求串行
Bridge 被搜索占满
播放重复 Resolve
HTTP connection 生命周期过短
```

---

# 47. 最终判断标准

完成本轮优化后，应该能够做到：

```text
搜索
    ↓
Bridge 稳定

搜索过程中
    ↓
页面仍然流畅

点击播放
    ↓
不会重新搜索

播放过程中
    ↓
不会因为搜索导致 Bridge 被占满

重复播放同一 Episode
    ↓
不会重复 Resolve

视频播放
    ↓
Gateway 持续流式传输

拖动进度
    ↓
Range 直接透传

Bridge 断线
    ↓
快速失败 + 自动恢复

Bridge 慢
    ↓
不会拖死 UI
```

这才算真正解决当前两个性能问题。