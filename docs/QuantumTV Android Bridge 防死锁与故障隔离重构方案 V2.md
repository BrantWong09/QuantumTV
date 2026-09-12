# QuantumTV Android Bridge 防死锁与故障隔离重构方案

> 本方案针对当前已经确认的核心问题：
>
> **Android Spider 的 `playerContent()` 可能进入不可中断的阻塞状态，导致 Spider 全局锁/线程池资源耗尽，最终整个 Bridge 无法响应。**
>
> 本方案不是简单调整 timeout，而是从执行隔离层解决问题。

---

# 1. 已确认根因

当前 Android Bridge 的实际问题：

```text
Desktop
   ↓
Bridge Request
   ↓
Android Spider
   ↓
Spider Executor
   ↓
playerContent()
   ↓
夸克 / wex
   ↓
native / blocking call
   ↓
永久或长时间不返回
```

Spider 端：

```text
全局锁
    +
固定 4 worker
```

因此：

```text
playerContent #1
    ↓
hang

playerContent #2
    ↓
hang

playerContent #3
    ↓
hang

playerContent #4
    ↓
hang
```

最终：

```text
Worker Pool
    ↓
0 available workers
```

此时：

```text
search
detail
playerContent
init
health
```

全部排队。

最终表现：

```text
搜索无结果
搜索转圈
播放一直转
Bridge timeout
重新搜索
重新连接
/init 无响应
```

---

# 2. 为什么 Desktop Timeout 无法解决

当前可能存在：

```rust
tokio::time::timeout(
    Duration::from_secs(8),
    bridge_request(...)
)
```

它只能做到：

```text
Desktop
   │
   ├── 等待 Spider
   │
   └── 8 秒后停止等待
```

但 Android：

```text
playerContent()
   ↓
仍然执行
```

也就是说：

```text
Desktop timeout
        ≠
Android task cancelled
```

最终形成：

```text
Desktop
   timeout
      ↓
retry
      ↓
new Android task
      ↓
old Android task still alive
```

这就是所谓：

> 僵尸调用。

---

# 3. Java Thread Interrupt 也不能作为最终方案

不能依赖：

```java
Future.cancel(true)
```

或者：

```java
Thread.interrupt()
```

解决问题。

因为：

```text
Java blocking code
```

可能响应 interrupt。

但是：

```text
JNI
native library
blocking socket
third-party SDK
native HTTP
```

不一定响应。

如果：

```text
playerContent()
```

已经进入 native：

```text
Thread.interrupt()
```

可能什么都做不了。

因此：

> **必须具备“杀死执行环境”的能力，而不是仅仅取消 Future。**

---

# 4. 新架构

Android 端必须拆成：

```text
                    Android App
                         │
              ┌──────────┴──────────┐
              │                     │
       Bridge Control          Spider Worker
              │                     │
              │               独立进程
              │                     │
              │              Spider Executor
              │                     │
              │              playerContent
              │                     │
              │              searchContent
              │                     │
              │              detailContent
              │
              └──── Watchdog ───────┘
```

核心原则：

> **Bridge Control 进程永远不能被 Spider 卡死。**

---

# 5. Android Process 隔离

推荐使用 Android 独立进程：

```xml
<service
    android:name=".bridge.SpiderWorkerService"
    android:process=":spider_worker"
    android:exported="false" />
```

主进程：

```text
com.quantumtv.android
```

Spider：

```text
com.quantumtv.android:spider_worker
```

这样：

```text
Spider 卡死
```

不会直接导致：

```text
Bridge Control 卡死
```

---

# 6. 为什么必须使用独立进程

当前：

```text
Main Process
 ├── Bridge
 ├── HTTP
 ├── Spider
 ├── Executor
 └── playerContent
```

一旦：

```text
playerContent
```

出现严重 native 卡死：

```text
Main Process
```

可能整体失去响应。

改成：

```text
Main Process
 ├── Bridge
 ├── Control
 └── Watchdog

Spider Process
 ├── Spider
 └── Worker
```

如果：

```text
Spider Process
```

死锁：

```text
kill spider process
        ↓
restart
        ↓
Bridge Control
仍然正常
```

这是整个方案最核心的一步。

---

# 7. Control Plane

主进程只负责：

```text
Bridge connection
Request routing
Worker lifecycle
Health
Watchdog
Authentication/session state
```

不允许主进程直接执行：

```text
Spider.playerContent()
Spider.searchContent()
Spider.detailContent()
```

---

# 8. Spider Worker

Spider Worker 专门执行：

```text
searchContent
detailContent
categoryContent
playerContent
```

Worker 接收：

```json
{
  "request_id": 1001,
  "method": "playerContent",
  "source": "wex",
  "flag": "quark",
  "id": "xxx"
}
```

返回：

```json
{
  "request_id": 1001,
  "status": "success",
  "result": {}
}
```

---

# 9. Worker 必须有硬超时

例如：

```text
search:
8s

detail:
8s

playerContent:
12s

init:
5s
```

但是：

> 这个 timeout 的意义不是“取消线程”。

而是：

```text
Worker watchdog timeout
        ↓
Worker process unhealthy
        ↓
kill worker process
        ↓
restart worker
```

---

# 10. PlayerContent 必须设置更严格的隔离

例如：

```text
playerContent timeout = 10~15s
```

一旦：

```text
15s
```

没有返回：

```text
Worker
 ↓
UNHEALTHY
```

不要继续：

```text
等待
```

更不能：

```text
重新调用 playerContent
```

应该：

```text
terminate worker
restart worker
```

---

# 11. Worker Pool

不要恢复成：

```text
一个 Worker
4 threads
```

因为这只是：

```text
一个进程
4 个可能一起死锁的线程
```

更推荐：

```text
Worker Process Pool

worker-1
    1 active request

worker-2
    1 active request
```

第一阶段：

```text
MAX_WORKERS = 2
```

---

# 12. 为什么不是一个进程 4 个线程

因为：

```text
playerContent #1
    ↓
native hang
```

会占用：

```text
worker thread
```

如果 4 个线程：

```text
4 次 hang
```

整个进程失去执行能力。

而：

```text
worker-1
```

死掉以后：

```text
worker-2
```

仍然可以处理搜索。

---

# 13. 推荐 Worker Pool

初始：

```text
Worker 1
Worker 2
```

每个：

```text
最多 1 个 Spider 调用
```

因此：

```text
最大并发 = 2
```

不是：

```text
2 processes × 4 threads
```

而是：

```text
2 processes × 1 active request
```

---

# 14. 请求调度

建议：

```text
Priority Queue
```

优先级：

```text
P0 Playback Resolve

P1 Detail

P2 Search

P3 Background
```

例如：

```text
Worker 1
    ↓
Search

Worker 2
    ↓
playerContent
```

如果：

```text
Worker 1
```

发生：

```text
playerContent hang
```

只影响：

```text
Worker 1
```

Worker 2：

```text
仍然可以 Search
```

---

# 15. 搜索不能和播放共用同一个 Worker

这是本轮必须修掉的问题。

错误：

```text
所有 Spider 请求
        ↓
同一个 Executor
```

正确：

```text
BridgeScheduler
        │
        ├── Playback Queue
        │       ↓
        │    Worker Pool
        │
        └── Search Queue
                ↓
             Worker Pool
```

---

# 16. 最简单实现

第一版甚至不需要复杂的多队列。

可以：

```text
Worker 1
    ↓
Playback

Worker 2
    ↓
Search / Detail
```

即：

```text
Dedicated Playback Worker
Dedicated General Worker
```

优点：

如果：

```text
夸克 playerContent
```

卡死：

```text
Playback Worker
```

直接死亡。

但是：

```text
Search Worker
```

完全不受影响。

---

# 17. 推荐架构

我更推荐：

```text
                    Scheduler
                       │
            ┌──────────┴──────────┐
            │                     │
       Playback Worker       General Workers
            │                  │        │
            │               Worker1  Worker2
            │
        playerContent
```

这样最符合你的实际场景。

---

# 18. Worker 状态

每个 Worker：

```rust
WorkerState {
    id,
    pid,
    status,
    current_request,
    started_at,
    last_heartbeat,
}
```

状态：

```text
Starting
Idle
Busy
Suspect
Killing
Dead
Restarting
```

---

# 19. Heartbeat

Worker 每：

```text
1s
```

发送：

```json
{
  "worker_id": "w1",
  "state": "busy",
  "request_id": 123,
  "timestamp": 123456
}
```

Control：

```text
last_heartbeat
```

超过：

```text
3s
```

不要立即杀。

标记：

```text
Suspect
```

超过：

```text
5s
```

并且当前请求已经超过 timeout：

```text
Kill
```

---

# 20. Watchdog

Watchdog 必须在：

```text
Spider Worker
```

外部。

禁止：

```text
Worker 自己 watchdog 自己
```

因为：

```text
Worker 卡死
```

以后：

```text
Worker 自己也无法执行 watchdog。
```

正确：

```text
Main Process
    │
    └── Watchdog
            │
            ├── worker-1
            └── worker-2
```

---

# 21. Kill Worker

当：

```text
playerContent
```

超过硬 timeout：

```text
Worker Process
        ↓
SIGKILL / Android Process.killProcess
        ↓
process dead
```

然后：

```text
spawn worker
```

---

# 22. Android Process Restart

不要：

```text
杀进程后等待用户重新打开 App
```

而应该：

```text
Worker died
    ↓
Control detects
    ↓
Start Worker
    ↓
Worker init
    ↓
Ready
```

目标：

```text
< 2~3 秒
```

---

# 23. `/init` 必须从 Worker 中剥离

你现在出现：

> 第二次会话连 `/init` 都没响应。

这其实是非常重要的证据。

说明：

```text
/init
```

当前依赖：

```text
Spider Worker
```

因此 Spider 死锁：

```text
/init
```

也死。

必须改成：

```text
Bridge /init
      ↓
Control Process
      ↓
返回 Bridge 信息
```

而不是：

```text
/init
 ↓
Spider
 ↓
Spider init
```

---

# 24. `/health` 同样不能依赖 Spider

错误：

```text
GET /health
 ↓
Spider.health()
```

如果 Spider 已经死：

```text
health 也死
```

正确：

```text
GET /health
 ↓
Bridge Control
 ↓
{
    bridge: healthy,
    worker: degraded
}
```

例如：

```json
{
  "bridge": "healthy",
  "worker": "degraded",
  "workers": {
    "playback": "dead",
    "general": "healthy"
  }
}
```

---

# 25. Bridge Health 三层状态

建议：

```text
Bridge
Worker
Request
```

分别统计。

例如：

```text
Bridge:
Healthy

General Worker:
Healthy

Playback Worker:
Restarting
```

这时候：

```text
搜索
```

应该：

```text
正常
```

而：

```text
播放
```

暂时：

```text
等待 Worker 恢复
```

---

# 26. 最关键的隔离

最终必须做到：

```text
Quark playerContent hang
        ↓
Playback Worker
        ↓
process killed
```

不能变成：

```text
Quark playerContent hang
        ↓
Android App
        ↓
Bridge
        ↓
Search
        ↓
整个应用
```

---

# 27. Spider Global Lock 必须移除

当前：

```text
global lock
```

必须重点检查。

如果存在：

```java
synchronized
```

或者：

```java
synchronized(spider)
```

或者：

```java
Mutex<Spider>
```

导致：

```text
playerContent
```

持锁：

```text
playerContent()
{
    synchronized(spider) {
        nativeCall();
    }
}
```

这是危险的。

因为：

```text
nativeCall()
```

挂死：

```text
lock
```

永久无法释放。

---

# 28. Spider 调用必须缩小锁范围

错误：

```java
synchronized (spider) {
    spider.playerContent(...);
}
```

正确：

```text
不要用全局锁保护整个调用。
```

如果第三方 Spider 本身不是线程安全的：

```text
不要靠大锁解决。
```

应该：

```text
一个 Worker Process
    ↓
一个 Spider instance
    ↓
一次一个调用
```

通过进程级隔离保证安全。

---

# 29. Worker 内部不需要 4 个线程

新的 Worker：

```text
Spider Worker
    ↓
Single Request Executor
```

即：

```text
1 process
1 spider
1 active call
```

这样非常容易推理：

```text
调用卡死
    ↓
杀进程
```

没有：

```text
锁
线程池
僵尸线程
```

这些复杂状态。

---

# 30. 为什么单 Worker 反而更稳定

Spider 的问题不是：

```text
CPU 不够
```

而是：

```text
Spider 本身可能 blocking
```

增加：

```text
4 threads
8 threads
16 threads
```

只会：

```text
加速把自己锁死。
```

因此：

```text
Process Isolation
>
Thread Concurrency
```

---

# 31. Desktop Bridge 不再无限 Retry

当前最危险的行为：

```text
Request timeout
    ↓
retry
    ↓
request timeout
    ↓
retry
```

如果 Android worker 已经卡死：

```text
retry
```

只是在：

```text
制造更多请求
```

必须：

```text
Request Timeout
 ↓
标记 Worker Suspect
 ↓
停止该 Worker 的新请求
 ↓
等待 Watchdog
 ↓
kill/restart
```

---

# 32. Retry Policy

建议：

```text
Search
最多 1 次 retry

Detail
最多 1 次 retry

playerContent
0 次自动 retry
```

特别是：

```text
playerContent timeout
```

禁止立即再次调用：

```text
playerContent
```

应该：

```text
Kill Worker
Restart Worker
```

然后由上层决定：

```text
是否重新 Resolve 一次
```

---

# 33. Search 的特殊处理

搜索失败：

```text
Worker timeout
```

可以：

```text
restart worker
```

然后：

```text
retry once
```

但必须：

```text
新的 Worker
```

不能：

```text
原 Worker
```

继续 retry。

---

# 34. Request ID 必须关联 Worker

日志：

```text
[Bridge]
request=1001
worker=playback-1
method=playerContent
```

如果超时：

```text
[Bridge]
request=1001
worker=playback-1
timeout=12s
action=kill_worker
```

然后：

```text
[Bridge]
worker=playback-1
pid=1234
status=killed
```

再：

```text
[Bridge]
worker=playback-1
pid=1250
status=ready
```

这样以后定位问题会非常容易。

---

# 35. Zombie Request 必须清理

Desktop：

```text
pending[request_id]
```

Android：

```text
worker request
```

两边都需要生命周期。

如果：

```text
Desktop timeout
```

必须：

```text
pending.remove(request_id)
```

如果：

```text
Worker killed
```

必须：

```text
worker.current_request
```

标记：

```text
WorkerKilled
```

返回：

```text
BridgeWorkerReset
```

而不是：

```text
Timeout
```

---

# 36. 新错误模型

增加：

```rust
BridgeError::WorkerTimeout

BridgeError::WorkerKilled

BridgeError::WorkerRestarting

BridgeError::WorkerUnavailable

BridgeError::SpiderTimeout

BridgeError::SpiderCrash
```

这样前端可以区分：

```text
网络错误
```

和：

```text
Spider 执行环境故障
```

---

# 37. 前端不能看到 Worker Timeout 后疯狂重试

例如：

```text
WorkerRestarting
```

前端显示：

```text
正在恢复播放服务...
```

而不是：

```text
不停 invoke()
```

搜索则：

```text
Bridge 正在恢复
```

等待：

```text
WorkerReady
```

再恢复请求。

---

# 38. Bridge Recovery State

建议：

```text
Normal
 ↓
WorkerDegraded
 ↓
WorkerRestarting
 ↓
WorkerReady
```

前端通过事件获得：

```text
bridge.worker_status
```

而不是：

```text
setInterval 100ms
```

轮询。

---

# 39. Worker Ready Event

Worker 启动成功：

```json
{
  "event": "worker_ready",
  "worker": "general",
  "pid": 12345
}
```

播放 Worker：

```json
{
  "event": "worker_ready",
  "worker": "playback",
  "pid": 12346
}
```

Desktop 收到后：

```text
允许发送请求
```

---

# 40. Worker Restart 冷却

不要：

```text
kill
restart
kill
restart
```

如果夸克每次：

```text
playerContent
```

都会导致：

```text
15 秒
→ kill
→ restart
```

会形成重启风暴。

因此：

```text
Crash Loop Protection
```

例如：

```text
1 次失败
正常 restart

2 次 / 30s
restart

3 次 / 60s
Worker Disabled
```

---

# 41. Source 级熔断

这是非常值得加的。

如果：

```text
wex / 夸克
```

连续：

```text
3 次 playerContent timeout
```

则：

```text
wex
```

进入：

```text
SourceDegraded
```

短时间内：

```text
不再调用 wex playerContent
```

但：

```text
其他 Spider
```

继续工作。

---

# 42. Source Circuit Breaker

例如：

```text
wex
 ↓
timeout
 ↓
timeout
 ↓
timeout
 ↓
OPEN
```

状态：

```text
Closed
Open
HalfOpen
```

等待：

```text
30s
```

后：

```text
HalfOpen
```

只允许：

```text
1 request
```

成功：

```text
Closed
```

失败：

```text
Open
```

---

# 43. 这会直接解决你现在的夸克问题

当前：

```text
夸克 playerContent
 ↓
hang
 ↓
占 worker
```

优化后：

```text
夸克 playerContent
 ↓
Playback Worker
 ↓
12s
 ↓
kill process
 ↓
restart
```

然后：

```text
wex
```

连续失败：

```text
Source Circuit Open
```

于是：

```text
搜索
```

仍然可以正常使用其他来源。

---

# 44. 搜索与播放彻底解耦

最终：

```text
                         Bridge
                           │
                       Scheduler
                           │
            ┌──────────────┴──────────────┐
            │                             │
      General Worker Pool          Playback Worker
            │                             │
       Search / Detail               playerContent
            │                             │
            ▼                             ▼
       Spider Worker                Spider Worker
            │                             │
         进程 A                         进程 B
```

如果：

```text
B
```

死：

```text
A
```

仍然工作。

---

# 45. `/init` 新设计

`/init` 不得执行：

```text
Spider.init()
```

如果确实需要：

```text
Spider.init()
```

则应该：

```text
/init
 ↓
Bridge Control
 ↓
快速返回 Bridge Ready
 ↓
后台初始化 Worker
```

例如：

```json
{
  "bridge": "ready",
  "worker": "starting"
}
```

然后：

```text
worker_ready
```

事件到达。

---

# 46. Bridge 启动流程

正确：

```text
Android App
    ↓
Bridge Control Start
    ↓
TCP Server Start
    ↓
Health Ready
    ↓
Spawn General Worker
    ↓
Spawn Playback Worker
    ↓
Worker Ready
```

不要：

```text
Bridge Start
 ↓
等待所有 Spider init
 ↓
Spider init
 ↓
卡死
 ↓
Bridge 无响应
```

---

# 47. Worker 启动失败

如果：

```text
Spider Worker
```

无法启动：

```text
Bridge Control
```

仍然保持：

```text
Healthy
```

但是：

```text
worker=unavailable
```

这样 Desktop 至少可以：

```text
获取状态
显示错误
重新连接
```

---

# 48. Bridge Protocol 增加 Worker 状态

建议：

```json
{
  "bridge": "healthy",
  "workers": [
    {
      "id": "general",
      "state": "ready"
    },
    {
      "id": "playback",
      "state": "restarting"
    }
  ]
}
```

---

# 49. Android Worker 不允许持有 Bridge TCP Socket

非常重要。

不要：

```text
Worker Process
    ↓
直接持有 Desktop TCP
```

应该：

```text
Desktop
   ↓
Control Process
   ↓
IPC
   ↓
Worker Process
```

这样：

```text
Worker crash
```

不会导致：

```text
TCP connection
```

也跟着丢失。

---

# 50. IPC

Android 主进程和 Worker：

优先：

```text
Binder
```

或者：

```text
LocalSocket
```

如果当前实现简单，可以：

```text
Binder Service
```

因为 Android 原生支持：

```text
Service
IPC
Process isolation
```

---

# 51. 如果当前 Spider 是 Java/Kotlin

推荐：

```text
SpiderWorkerService
```

作为独立进程 Service。

结构：

```text
Main Process
    │
    ├── BridgeServer
    ├── BridgeScheduler
    └── WorkerManager
             │
             ├── bindService
             │
             ▼
       :spider_worker
             │
        SpiderWorkerService
             │
        SpiderExecutor
             │
           Spider
```

---

# 52. 如果 Spider 包含 native library

这种方案更加重要。

因为：

```text
JNI
native HTTP
native browser
native SDK
```

一旦发生：

```text
native deadlock
```

Java：

```text
Thread.interrupt()
```

可能无效。

但是：

```text
Process.killProcess()
```

可以直接清理：

```text
Java Thread
JNI
Native Thread
Native Memory
Locks
Sockets
```

然后重新启动干净的 Worker。

---

# 53. Worker 内存也会被一起清理

这还有一个额外收益：

```text
Spider
 ↓
native memory leak
```

如果长期运行：

```text
RSS
↑
↑
↑
```

Worker 重启：

```text
Process Exit
 ↓
OS 回收全部内存
```

所以：

> Worker Process 本身就是一个天然的资源回收边界。

---

# 54. 不要做成“每个请求一个进程”

虽然最安全：

```text
request
 ↓
new process
 ↓
Spider
 ↓
kill
```

但启动成本太高。

不推荐。

使用：

```text
2 个长期 Worker Process
```

即可。

只有：

```text
timeout
crash
deadlock
```

才重启 Worker。

---

# 55. Worker Pool 初始参数

建议第一版：

```text
General Worker:
1

Playback Worker:
1

Max concurrent:
1 / worker
```

总共：

```text
2 processes
2 active Spider calls
```

先稳定。

不要一开始：

```text
4 Worker
8 Worker
16 Worker
```

---

# 56. 后续再扩容

如果稳定：

```text
General Worker:
2

Playback Worker:
1
```

即：

```text
Search concurrency = 2
Playback concurrency = 1
```

已经足够大部分桌面播放器使用。

---

# 57. Spider Worker 不应该共享全局锁

必须重点排查：

```text
static synchronized
static Mutex
singleton Spider
global executor
```

如果存在：

```text
GLOBAL_SPIDER_LOCK
```

删除或者缩小作用域。

最终模型：

```text
Worker Process
    ↓
one Spider instance
    ↓
one active call
```

根本不需要：

```text
global lock
```

---

# 58. Search 不应该因为 Playback 卡死而失败

这是最重要的验收条件之一。

测试：

```text
1. 打开夸克播放
2. playerContent 故意让它 hang
3. 等待 15 秒
4. 同时搜索“测试”
```

预期：

```text
Playback Worker
    ↓
timeout
    ↓
kill
    ↓
restart

General Worker
    ↓
search
    ↓
正常返回
```

---

# 59. 第二个核心测试

测试：

```text
1. wex playerContent hang
2. Desktop timeout
3. 不重启 Desktop
4. 搜索其他来源
```

预期：

```text
搜索正常
```

---

# 60. 第三个核心测试

测试：

```text
1. wex playerContent hang
2. Playback Worker kill
3. Worker restart
4. 调用 /health
```

预期：

```json
{
  "bridge": "healthy",
  "playback_worker": "ready"
}
```

而不是：

```text
/init timeout
```

---

# 61. 第四个核心测试

连续制造：

```text
wex playerContent
timeout
timeout
timeout
```

预期：

```text
Worker
restart
restart
restart
```

最终：

```text
wex circuit breaker = OPEN
```

而：

```text
search
```

仍然：

```text
正常
```

---

# 62. 第五个核心测试

Worker 被 kill：

```text
Desktop TCP
```

不能断。

也就是说：

```text
Android Control Process
```

继续：

```text
accept request
```

只是返回：

```text
WorkerRestarting
```

---

# 63. Desktop Bridge 状态

增加：

```text
BridgeState
```

例如：

```text
Ready

Degraded

WorkerRestarting

Unavailable
```

其中：

```text
WorkerRestarting
```

不等于：

```text
BridgeUnavailable
```

---

# 64. Desktop 请求策略

如果：

```text
worker = restarting
```

Search：

```text
等待最多 3s
```

如果：

```text
worker ready
```

立即执行。

Playback：

```text
等待 WorkerReady
```

不要：

```text
创建新的 Android 请求
```

---

# 65. 取消机制

当用户：

```text
搜索 A
```

然后：

```text
搜索 B
```

A 应该：

```text
cancel
```

但是如果 A 已经进入：

```text
native Spider
```

无法取消：

```text
不要等待它。
```

交给：

```text
Worker watchdog
```

最终：

```text
timeout
→ kill worker
```

---

# 66. 这比“取消线程”更加可靠

完整模型：

```text
可取消阶段
    ↓
Request cancellation

不可取消阶段
    ↓
Worker timeout

严重阻塞
    ↓
Process kill
```

这三个层次必须区分。

---

# 67. Request Timeout 与 Worker Timeout

不要混为一个值。

例如：

```text
Request Timeout
= 8s
```

表示：

```text
Desktop 不再等待
```

而：

```text
Worker Hard Timeout
= 15s
```

表示：

```text
Android Worker 必须被杀死
```

因此：

```text
8s
Desktop 放弃

15s
Worker kill
```

---

# 68. 为什么要留 7 秒间隔

例如：

```text
Desktop:
8s timeout

Worker:
15s hard timeout
```

中间：

```text
7s
```

用于：

```text
cleanup
```

但是：

```text
如果 worker 已经明显卡死
```

可以提前 kill。

---

# 69. 最终超时层级

推荐：

```text
Frontend
   5~8s

Desktop Bridge
   8~10s

Android Worker
   12~15s

Network
   5~10s connect

Native Spider
   不可控
```

最重要：

```text
Native Spider
```

没有可靠 timeout：

```text
Worker Process
```

就是最终保险丝。

---

# 70. Metrics

必须记录：

```text
worker_id
pid
request_id
source
method
start_time
queue_time
execution_time
timeout
kill
restart
```

例如：

```text
[SpiderWorker]
worker=playback
pid=14231
method=playerContent
source=wex
duration=15023ms
status=timeout
action=kill
```

---

# 71. 关键统计指标

增加：

```text
worker_timeout_total

worker_restart_total

worker_crash_total

worker_busy_seconds

spider_request_total

spider_request_timeout

spider_request_cancel

source_timeout_total
```

特别观察：

```text
wex.playerContent.timeout
```

---

# 72. Source 级统计

例如：

```text
Baidu
playerContent:
P50 = 400ms
P95 = 800ms
timeout = 0

Wex
playerContent:
P50 = 2s
P95 = 5s
timeout = 17
```

这时候就非常清楚：

```text
不是 Bridge 网络慢
```

而是：

```text
Wex Spider
```

有问题。

---

# 73. 绝对不要通过提高线程数量解决

禁止：

```text
4 → 8
8 → 16
```

因为：

```text
4 threads
```

已经可以：

```text
4 个死锁
```

增加线程：

```text
16 threads
```

只会：

```text
16 个死锁
```

然后：

```text
CPU
Memory
FD
Socket
```

全部被拖垮。

---

# 74. 绝对不要继续提高 Timeout

禁止：

```text
60s
120s
180s
```

这种做法。

因为：

```text
playerContent hang
```

不是：

```text
网络慢
```

而是：

```text
调用没有返回
```

timeout 越大：

```text
僵尸生命周期越长
```

---

# 75. Bridge 最终架构

最终：

```text
                         Desktop
                            │
                       Bridge Client
                            │
                            ▼
                    Android Control
                            │
                  ┌─────────┴─────────┐
                  │                   │
             General Worker      Playback Worker
                  │                   │
              Process A           Process B
                  │                   │
              Spider A             Spider B
                  │                   │
              search/detail      playerContent
```

如果：

```text
Process B
```

挂：

```text
Process A
```

完全不受影响。

---

# 76. 再进一步：Worker Supervisor

最终 Android：

```text
BridgeSupervisor
```

负责：

```text
start worker
stop worker
restart worker
health
timeout
metrics
circuit breaker
```

目录：

```text
android/
    bridge/
        BridgeServer
        BridgeSupervisor
        WorkerManager
        WorkerProcess
        WorkerProtocol
        WorkerHealth
        WorkerWatchdog
        CircuitBreaker
```

---

# 77. 推荐代码结构

如果 Android 工程允许：

```text
bridge/
├── BridgeServer.kt
├── BridgeProtocol.kt
├── BridgeScheduler.kt
│
├── supervisor/
│   ├── WorkerManager.kt
│   ├── WorkerState.kt
│   ├── WorkerWatchdog.kt
│   └── CircuitBreaker.kt
│
└── worker/
    ├── SpiderWorkerService.kt
    ├── SpiderWorker.kt
    └── WorkerProtocol.kt
```

---

# 78. 第一阶段不要改 Spider 本身

第一阶段目标：

```text
隔离
```

先实现：

```text
Control Process
+
Worker Process
+
Watchdog
+
Process Restart
```

不要一开始去修改：

```text
Wex Spider
```

因为：

```text
第三方 Spider
```

可能：

```text
不可修改
```

或者：

```text
修改后升级困难
```

---

# 79. 第二阶段再处理 Spider 锁

等进程隔离完成：

检查：

```text
global lock
synchronized
executor
singleton
```

然后：

```text
one worker
one spider
one request
```

尽量删除：

```text
全局 Spider Lock
```

---

# 80. 第三阶段增加 Source Circuit Breaker

完成：

```text
Worker Isolation
```

以后：

```text
wex
```

连续失败：

```text
Open
```

避免：

```text
每次播放都把 Worker 再杀一次。
```

---

# 81. 第四阶段优化 Bridge Pool

等稳定之后：

```text
General Worker
×2
```

然后：

```text
Search concurrency
```

提高。

不要反过来。

---

# 82. 与上一轮 Bridge 优化的关系

上一轮提出：

```text
BridgeSession
SearchCache
ResolveSingleFlight
PlaybackPriority
```

依然有效。

但必须调整优先级：

### P0

```text
Android Worker Process Isolation
```

### P0

```text
Watchdog
```

### P0

```text
Worker Restart
```

### P0

```text
Search / Playback Worker 隔离
```

### P0

```text
禁止 timeout 后无限 retry
```

然后才是：

```text
P1
BridgeSession

P1
SearchCache

P1
ResolveSingleFlight
```

---

# 83. 最终请求链

搜索：

```text
Search
 ↓
Desktop BridgeSession
 ↓
Android Control
 ↓
General Worker
 ↓
Spider
 ↓
Result
```

播放：

```text
Play
 ↓
Desktop BridgeSession
 ↓
Android Control
 ↓
Playback Worker
 ↓
Spider
 ↓
MediaResource
```

如果：

```text
Playback Worker
```

死：

```text
Playback Worker
 ↓
kill
 ↓
restart
```

而：

```text
General Worker
```

完全继续工作。

---

# 84. 最终故障链

如果：

```text
Wex playerContent
```

挂死：

```text
Wex
 ↓
playerContent
 ↓
hang
 ↓
Playback Worker timeout
 ↓
kill process
 ↓
new Playback Worker
 ↓
ready
```

如果连续发生：

```text
Wex timeout × 3
 ↓
Wex Circuit Open
```

然后：

```text
搜索
 ↓
General Worker
 ↓
正常
```

---

# 85. 最重要的验收标准

必须满足：

```text
① 一个 playerContent 永久 hang
```

不能影响：

```text
搜索
详情
其他 Spider
Bridge health
/init
```

---

```text
② Worker 永久 hang
```

必须：

```text
15s 内被检测
↓
kill
↓
restart
```

---

```text
③ Worker restart
```

不能：

```text
Desktop Bridge TCP 断开
```

---

```text
④ 连续 3 次 Wex hang
```

必须：

```text
Wex Circuit Open
```

但：

```text
其他来源继续工作
```

---

```text
⑤ Desktop App 重启
```

Android Worker 如果仍然存活：

```text
必须重新建立正常 Bridge Session
```

如果 Worker 状态异常：

```text
Supervisor 自动恢复
```

---

# 86. 最终禁止事项

AI Coding Agent 禁止：

```text
❌ 把 timeout 从 8s 改成 60s

❌ 增加 Spider ThreadPool 到 8/16/32

❌ 用 Future.cancel(true) 当最终解决方案

❌ 用 Thread.interrupt() 当最终解决方案

❌ timeout 后继续向同一个 Worker retry

❌ playerContent 和 search 共用一个 Worker

❌ Spider Worker 持有 Desktop TCP 长连接

❌ /init 依赖 Spider Worker

❌ /health 依赖 Spider Worker

❌ Worker 自己负责自己的 watchdog

❌ 一个 Spider 全局锁保护所有请求

❌ Worker 崩溃导致 Bridge Control 一起退出
```

---

# 87. AI Coding Agent 执行顺序

必须严格按照：

```text
Step 1
确认当前 Android Bridge 进程结构

↓

Step 2
确认 Spider Executor / 全局锁

↓

Step 3
确认 playerContent 调用链

↓

Step 4
确认 /init 调用链

↓

Step 5
确认 /health 调用链

↓

Step 6
实现 Worker Process

↓

Step 7
实现 Worker IPC

↓

Step 8
实现 WorkerManager

↓

Step 9
实现 Watchdog

↓

Step 10
实现 Worker Kill + Restart

↓

Step 11
拆分 General / Playback Worker

↓

Step 12
实现 Source Circuit Breaker

↓

Step 13
最后再优化 Desktop Bridge
```

---

# 88. 最终目标

QuantumTV Bridge 不应该追求：

```text
“Spider 永远不出错”
```

这是不现实的。

真正应该追求：

```text
Spider 可以出错
        ↓
但错误不能传播
        ↓
Worker 可以挂
        ↓
Worker 可以被杀
        ↓
Worker 可以自动恢复
        ↓
Bridge 永远保持可控
        ↓
搜索永远不会因为播放解析挂死
```

最终形成：

```text
                ┌───────────────────────┐
                │     Bridge Control    │
                │                       │
                │  Session              │
                │  Scheduler            │
                │  Health               │
                │  Supervisor            │
                └───────────┬───────────┘
                            │
                 ┌──────────┴──────────┐
                 │                     │
          General Worker        Playback Worker
                 │                     │
              Process A             Process B
                 │                     │
               Spider                Spider
                 │                     │
          Search / Detail         playerContent
```

**最重要的一句话：**

> **不要试图让一个可能卡死的 Spider 变得“不会卡死”，而应该让它即使卡死，也只能杀死自己，而不能杀死整个 Bridge。**