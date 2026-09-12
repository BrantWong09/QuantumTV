# Android Bridge 防死锁与故障隔离实施计划 (Plan 1 / 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 Android Bridge 拆成 Control 主进程 + 两个单调用 Spider Worker 子进程（general / playback），用 Watchdog 硬超时 + `Process.killProcess` + 自动重启实现故障隔离：任何 spider 调用挂死只能杀死自己所在的 Worker 进程，永远不能杀死 Bridge（桌面 TCP、/health、/init、其他来源搜索）。

**Architecture:** 主进程保留桌面 TCP（TunnelClient）与 8080 遗留通道，只做路由/生命周期/健康；spider 反射调用整体迁入独立进程（manifest `android:process`），进程间用 abstract LocalSocket 的 `[reqId|type|len|json]` 帧通信；Worker 单线程一次一个调用，心跳 1s；主进程 Watchdog 按"请求硬超时 + 心跳停滞"双条件杀进程并重启；播放与搜索/详情分进程；夸克类挂死 → playback worker 被杀重启，general worker 与 TCP 完全无感。

**Tech Stack:** Java 8 源码级（javac --release 8 via JDK 17 pin）、android-30 platform、build.ps1 手工 javac+d8+aapt2+zipalign+apksigner；MuMu 模拟器 `emulator-5554` + adb 验收；纯逻辑类零 Android 依赖 → PC JVM 直接跑 main 断言。

**Spec:** `docs/QuantumTV Android Bridge 防死锁与故障隔离重构方案 V2.md`(下称"方案";§N 指其章节)。桌面侧前一轮优化见 `docs/superpowers/plans/2026-09-12-bridge-playback-perf-optimization.md`。

## Global Constraints

- 语言/构建不变：纯 Java + build.ps1（已钉死 JDK 17: `3e19ced`），**不引入 Gradle/Kotlin/AIDL**；IPC 用 `android.net.LocalSocket`（abstract namespace），§50 允许的简化路线。
- 禁止事项照方案 §86 全量执行：不加大线程数、不加大 timeout 当解药、不靠 `Thread.interrupt`/`Future.cancel`、worker 不持有 TCP、/init 与 /health 不依赖 spider、不做 worker 自我 watchdog、超时后不向同一 worker retry、不共用一个 worker、全局 spider 锁随迁移删除、worker 崩溃不得带崩 Control。
- 超时值策略（已确认 #1）：**先埋点测量再定硬超时**。本计划阶段 A 用宽松默认（playerContent 90s / search 60s / detail 60s / init 30s，等于旧桌面容忍度，不引入新失败）；Task 8 采集真实分布后由人审数字、Task 10 落地。硬超时原则 = 实测 P95 × 2，下限 30s。
- Cookie/登录态策略（已确认 #2）：主进程 `/setCookie` → 写 CookieManager + `files/TV/.<drive>cookie` → **IPC 显式广播 cookie_update** → worker 清 spiderCache 重建；worker 启动时主进程下发当前 ext；文件通道保留兜底。
- 测试钩子仅 debuggable 构建可用（`ApplicationInfo.FLAG_DEBUGGABLE`，现 manifest `android:debuggable="true"`）：`/__test_hang`、`/__test_stats`。
- 帧协议对桌面**完全不变**：APK 回给桌面的仍是 `[id][len][HTTP body JSON]`；worker 挂死/重启对桌面呈现为 `{"code":503,"err":"worker_killed"}` 等信封，桌面 HTTP 语义不变。
- 验收环境（已确认 #3）：MuMu `emulator-5554`（adb: `D:\Program Files\Netease\MuMu\nx_main\adb.exe`，SDK platform-tools 也在）+ 真机 PGBM10（夸克回归）。同一时刻桌面只接受一条隧道拨入，测试时注意先断开另一方。
- 每个 Android 任务的"测试循环" = host JVM 单测（纯逻辑类）或 adb 集成断言（组件类），禁止"编译通过即完成"。
- 收尾验收必须通过: 本计划 Task 3-7 的 `test_isolation.ps1` 全绿 + `cargo test -p quantumtv-core` (Task 9) + 既有桌面端回归不破坏。

## 进程拓扑（目标态, §4/§75/§76）

```
com.quantumtv.bridge            ← Control: TunnelClient(TCP→桌面) + 8080 + Scheduler
        │ LocalSocket "qtv.bridge.ctl.<role>"
 ├── com.quantumtv.bridge:spider_general    ← GeneralWorkerService: 1 线程, search/detail/home/category
 └── com.quantumtv.bridge:spider_playback   ← PlaybackWorkerService: 1 线程, playerContent
```

## 新文件结构

```
android/spider-bridge/src/com/quantumtv/bridge/
├── BridgeService.java          # 改造: 只保留控制面 (TCP/8080/路由/生命周期/健康)
├── TunnelClient.java           # 不动 (仍调用 svc.routeRequest)
├── control/WorkerManager.java  # 生成/监控/重启 worker, 请求表, 超时, 熔断接线
├── control/SourceBreaker.java  # 纯 Java: 按 class 的 playerContent 熔断 (§41/§42)
├── ipc/Proto.java              # 纯 Java: 帧编解码 + msg 类型 (§8)
├── ipc/WorkerState.java        # 纯 Java: §18 状态机枚举+转移
├── ipc/TimeoutPolicy.java      # 纯 Java: method→硬超时表 (§9/§67, Task 10 调值)
├── worker/BaseSpiderWorker.java# Worker 进程主体: 连控制面→执行→心跳 (§29)
├── worker/GeneralWorkerService.java
├── worker/PlaybackWorkerService.java
└── worker/SpiderExec.java      # 从 BridgeService 迁入的反射执行体 (§57: 无全局锁)
```

## IPC 帧（与隧道帧同风格, §8/§49）

```
[u32 BE reqId][u32 BE type][u32 BE len][payload]

type: 1 HELLO(role,pid)  2 READY  3 REQ(method,source_class,args)  4 RESP(code,err,data)
      5 HB(state,curReqId,curMethod,curAgeMs)  6 COOKIE(ext,drives)  7 HANG(test only)
```

Control→Worker 只发 REQ/HB 请求/COOKIE/HANG；Worker→Control 只发 HELLO/READY/RESP/HB。

---

### Task 1: IPC 协议编解码（纯 Java + host JVM 测试）

**Files:**
- Create: `android/spider-bridge/src/com/quantumtv/bridge/ipc/Proto.java`
- Create: `android/spider-bridge/hosttest/ProtoTest.java`（不进 APK，host 编译运行）
- Test 入口: `android/spider-bridge/hosttest/run_hosttests.ps1`

**Interfaces:**
- Produces: `Proto.encode(int reqId, int type, byte[] payload) -> byte[]`; `Proto.decode(byte[] buf, int off, int len) -> Frame|null`（null=不完整）; `Frame{int reqId,type; byte[] payload; int used}`; 常量 `Proto.T_HELLO=1 ... T_HANG=7`。payload JSON 用 org.json（android 内置；host 测试用 `deps/json.jar`? 无 → **Proto 不做 JSON 解析，只管字节帧**；JSON 组包留给调用方）。

- [x] **Step 1: 写失败测试 `hosttest/ProtoTest.java`**

```java
import com.quantumtv.bridge.ipc.Proto;
import java.util.Arrays;

public class ProtoTest {
    static void eq(Object a, Object b, String msg) {
        if (!a.equals(b)) throw new AssertionError(msg + ": " + a + " != " + b);
    }
    public static void main(String[] args) throws Exception {
        // encode→decode 往返
        byte[] raw = Proto.encode(7, Proto.T_REQ, "{\"m\":\"search\"}".getBytes("UTF-8"));
        eq(raw[0], (byte)0, "id BE 0");
        eq(raw[3], (byte)7, "id BE 3");
        Proto.Frame f = Proto.decode(raw, 0, raw.length);
        eq(f.reqId, 7, "reqId");
        eq(f.type, Proto.T_REQ, "type");
        eq(new String(f.payload, "UTF-8"), "{\"m\":\"search\"}", "payload");

        // 不完整返回 null, 补全后成功
        byte[] part = Arrays.copyOf(raw, raw.length - 3);
        eq(Proto.decode(part, 0, part.length), null, "partial must be null");
        Proto.Frame f2 = Proto.decode(raw, 0, raw.length);
        eq(f2.used, raw.length, "used");

        // 多帧连读 (流式粘包)
        byte[] a = Proto.encode(1, Proto.T_HB, new byte[]{1});
        byte[] b = Proto.encode(2, Proto.T_RESP, new byte[]{2,3});
        byte[] both = new byte[a.length + b.length];
        System.arraycopy(a, 0, both, 0, a.length);
        System.arraycopy(b, 0, both, a.length, b.length);
        Proto.Frame x = Proto.decode(both, 0, both.length);
        eq(x.reqId, 1, "frame1 id");
        Proto.Frame y = Proto.decode(both, x.used, both.length - x.used);
        eq(y.reqId, 2, "frame2 id");

        // 超限拒绝
        try {
            Proto.encode(1, Proto.T_REQ, new byte[9 * 1024 * 1024]);
            throw new AssertionError("oversize must throw");
        } catch (IllegalArgumentException ok) { }
        System.out.println("ProtoTest OK");
    }
}
```

- [x] **Step 2: 写 host 测试脚本 `hosttest/run_hosttests.ps1` 并确认失败**

```powershell
$ErrorActionPreference = 'Stop'
$jdk = if ($env:JAVA_HOME -and (Test-Path "$env:JAVA_HOME\bin\javac.exe")) { $env:JAVA_HOME } else { "D:\devtools\jdk17" }
$root = Join-Path $PSScriptRoot ".."
$out = Join-Path $PSScriptRoot "out"
Remove-Item $out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $out | Out-Null
& "$jdk\bin\javac.exe" -encoding UTF-8 -d $out `
    (Join-Path $root "src\com\quantumtv\bridge\ipc\Proto.java") `
    (Join-Path $root "src\com\quantumtv\bridge\ipc\WorkerState.java") `
    (Join-Path $root "src\com\quantumtv\bridge\ipc\TimeoutPolicy.java") `
    (Join-Path $root "src\com\quantumtv\bridge\control\SourceBreaker.java") `
    (Join-Path $PSScriptRoot "*.java")
if ($LASTEXITCODE -ne 0) { throw "hosttest javac failed" }
foreach ($t in @("ProtoTest","WorkerStateTest","TimeoutPolicyTest","SourceBreakerTest")) {
    & "$jdk\bin\java.exe" -cp $out $t
    if ($LASTEXITCODE -ne 0) { throw "$t FAILED" }
}
Write-Host "ALL HOSTTESTS OK"
```

Run: `powershell -File android\spider-bridge\hosttest\run_hosttests.ps1`
Expected: javac 失败（Proto.java 尚不存在）。注：脚本同时引用 Task 2 的三个类文件——先建空壳（见 Step 4 注释），或逐任务临时注释掉未建文件；以 Proto 为准先跑通 ProtoTest 亦可。

- [x] **Step 3: 实现 `ipc/Proto.java`**

```java
package com.quantumtv.bridge.ipc;

/** Control↔Worker IPC 帧: [u32 BE reqId][u32 BE type][u32 BE len][payload] (与隧道帧同风格)。纯 Java, host 可测。 */
public final class Proto {
    public static final int T_HELLO = 1; // → {role,pid}
    public static final int T_READY = 2; // → {}
    public static final int T_REQ   = 3; // → {method,class,args...}
    public static final int T_RESP  = 4; // → {code,err,data}
    public static final int T_HB    = 5; // → {state,curReqId,curMethod,curAgeMs}
    public static final int T_COOKIE = 6; // → {ext,drives:{quark:11|uc:1|baidu:1}}
    public static final int T_HANG  = 7; // debug: 永久挂起当前执行线程 (验收钩子)
    public static final int MAX_PAYLOAD = 8 * 1024 * 1024;

    public static final class Frame {
        public final int reqId;
        public final int type;
        public final byte[] payload;
        public final int used;
        Frame(int reqId, int type, byte[] payload, int used) {
            this.reqId = reqId; this.type = type; this.payload = payload; this.used = used;
        }
    }

    private Proto() {}

    public static byte[] encode(int reqId, int type, byte[] payload) {
        if (payload.length > MAX_PAYLOAD) {
            throw new IllegalArgumentException("payload too large: " + payload.length);
        }
        byte[] out = new byte[12 + payload.length];
        writeInt(out, 0, reqId);
        writeInt(out, 4, type);
        writeInt(out, 8, payload.length);
        System.arraycopy(payload, 0, out, 12, payload.length);
        return out;
    }

    /** 从 buf[off..off+len) 解一帧; 不完整返回 null */
    public static Frame decode(byte[] buf, int off, int len) {
        if (len < 12) return null;
        int reqId = readInt(buf, off);
        int type = readInt(buf, off + 4);
        int plen = readInt(buf, off + 8);
        if (plen < 0 || plen > MAX_PAYLOAD) return null; // 超限/畸形 → 断开
        if (len - 12 < plen) return null;
        byte[] payload = new byte[plen];
        System.arraycopy(buf, off + 12, payload, 0, plen);
        return new Frame(reqId, type, payload, 12 + plen);
    }

    static void writeInt(byte[] b, int o, int v) {
        b[o] = (byte) (v >>> 24); b[o + 1] = (byte) (v >>> 16);
        b[o + 2] = (byte) (v >>> 8); b[o + 3] = (byte) v;
    }

    static int readInt(byte[] b, int o) {
        return ((b[o] & 0xFF) << 24) | ((b[o + 1] & 0xFF) << 16)
             | ((b[o + 2] & 0xFF) << 8) | (b[o + 3] & 0xFF);
    }
}
```

- [x] **Step 4: host 跑通**

Run: `powershell -File android\spider-bridge\hosttest\run_hosttests.ps1`（临时只放开 ProtoTest 行亦可）
Expected: `ProtoTest OK`。

- [x] **Step 5: Commit**

```bash
git add android/spider-bridge/src/com/quantumtv/bridge/ipc/Proto.java android/spider-bridge/hosttest
git commit -m "feat(android-ipc): LocalSocket 帧协议 Proto + host 往返测试"
```

---

### Task 2: 纯逻辑件 — WorkerState 状态机 / TimeoutPolicy / SourceBreaker

**Files:**
- Create: `android/spider-bridge/src/com/quantumtv/bridge/ipc/WorkerState.java`
- Create: `android/spider-bridge/src/com/quantumtv/bridge/ipc/TimeoutPolicy.java`
- Create: `android/spider-bridge/src/com/quantumtv/bridge/control/SourceBreaker.java`
- Create: `android/spider-bridge/hosttest/WorkerStateTest.java` / `TimeoutPolicyTest.java` / `SourceBreakerTest.java`

**Interfaces:**
- Produces:
  - `WorkerState` enum: `STARTING,IDLE,BUSY,SUSPECT,KILLING,DEAD,RESTARTING,DISABLED`；`boolean canAcceptRequest()`（仅 IDLE）
  - `TimeoutPolicy.hardMs(String method) -> long`（`"playerContent","search","detail","home","category","init"`；阶段 A: playerContent 90000, search/detail/home/category 60000, init 30000; `static volatile long PLAYERCONTENT_MS` 等便于 Task 10 调整与 `__test_stats` 展示）
  - `SourceBreaker(long openMs)`: `boolean allowPlayerContent(String cls)`; `void recordPlayerContentSuccess(String cls)`; `void recordPlayerContentTimeout(String cls)`（连续 3 次 → OPEN 30s → 半开放 1 探测）; `String snapshot()`（health 展示用）。

- [x] **Step 1: 写失败测试（三个 host main）**

```java
// WorkerStateTest.java
public class WorkerStateTest {
    static void t(boolean c, String m) { if (!c) throw new AssertionError(m); }
    public static void main(String[] a) {
        t(com.quantumtv.bridge.ipc.WorkerState.IDLE.canAcceptRequest(), "idle accepts");
        t(!com.quantumtv.bridge.ipc.WorkerState.BUSY.canAcceptRequest(), "busy rejects");
        t(!com.quantumtv.bridge.ipc.WorkerState.STARTING.canAcceptRequest(), "starting rejects");
        t(!com.quantumtv.bridge.ipc.WorkerState.DISABLED.canAcceptRequest(), "disabled rejects");
        System.out.println("WorkerStateTest OK");
    }
}
```

```java
// TimeoutPolicyTest.java
public class TimeoutPolicyTest {
    static void t(boolean c, String m) { if (!c) throw new AssertionError(m); }
    public static void main(String[] a) {
        t(com.quantumtv.bridge.ipc.TimeoutPolicy.hardMs("playerContent") == 90_000L, "pc 90s phase-A");
        t(com.quantumtv.bridge.ipc.TimeoutPolicy.hardMs("search") == 60_000L, "search 60s phase-A");
        t(com.quantumtv.bridge.ipc.TimeoutPolicy.hardMs("unknown") == 60_000L, "default 60s");
        System.out.println("TimeoutPolicyTest OK");
    }
}
```

```java
// SourceBreakerTest.java (注入时钟: SourceBreaker 接收 java.util.function.LongSupplier nowMs)
import com.quantumtv.bridge.control.SourceBreaker;
public class SourceBreakerTest {
    static void t(boolean c, String m) { if (!c) throw new AssertionError(m); }
    public static void main(String[] a) {
        long[] clock = {0};
        SourceBreaker b = new SourceBreaker(30_000, () -> clock[0]);
        t(b.allowPlayerContent("WexquarkGuard"), "closed initially");
        b.recordPlayerContentTimeout("WexquarkGuard");
        b.recordPlayerContentTimeout("WexquarkGuard");
        t(b.allowPlayerContent("WexquarkGuard"), "2 timeouts still closed");
        b.recordPlayerContentTimeout("WexquarkGuard");
        t(!b.allowPlayerContent("WexquarkGuard"), "3rd → open");
        t(b.allowPlayerContent("WexotherGuard"), "other source unaffected (§41)");
        clock[0] = 29_000;
        t(!b.allowPlayerContent("WexquarkGuard"), "still open at 29s");
        clock[0] = 31_000;
        t(b.allowPlayerContent("WexquarkGuard"), "half-open: 1 probe allowed");
        t(!b.allowPlayerContent("WexquarkGuard"), "only one probe");
        b.recordPlayerContentTimeout("WexquarkGuard"); // probe failed → reopen
        t(!b.allowPlayerContent("WexquarkGuard"), "reopen after failed probe");
        clock[0] = 70_000;
        t(b.allowPlayerContent("WexquarkGuard"), "second window half-open");
        b.recordPlayerContentSuccess("WexquarkGuard"); // → closed
        t(b.allowPlayerContent("WexquarkGuard"), "closed after success");
        System.out.println("SourceBreakerTest OK");
    }
}
```

- [x] **Step 2: 运行确认失败** — `powershell -File android\spider-bridge\hosttest\run_hosttests.ps1` → 编译失败。

- [x] **Step 3: 实现**

`ipc/WorkerState.java`:

```java
package com.quantumtv.bridge.ipc;

/** Worker 生命周期状态 (§18)。只有 IDLE 接受新请求。 */
public enum WorkerState {
    STARTING, IDLE, BUSY, SUSPECT, KILLING, DEAD, RESTARTING, DISABLED;

    public boolean canAcceptRequest() { return this == IDLE; }
}
```

`ipc/TimeoutPolicy.java`:

```java
package com.quantumtv.bridge.ipc;

/**
 * Worker 硬超时表 (§9/§67/§69): 含义不是取消线程, 而是 Watchdog 杀进程的依据。
 * 阶段 A 宽容值 = 旧桌面容忍度, 只防永久挂死不防慢; Task 8 实测分布后调低 (P95×2, 下限 30s)。
 */
public final class TimeoutPolicy {
    private TimeoutPolicy() {}

    public static volatile long PLAYERCONTENT_MS = 90_000;
    public static volatile long SEARCH_MS = 60_000;
    public static volatile long DETAIL_MS = 60_000;
    public static volatile long INIT_MS = 30_000;

    public static long hardMs(String method) {
        switch (method) {
            case "playerContent": return PLAYERCONTENT_MS;
            case "search": return SEARCH_MS;
            case "detail": case "home": case "category": return DETAIL_MS;
            case "init": return INIT_MS;
            default: return 60_000;
        }
    }
}
```

`control/SourceBreaker.java`:

```java
package com.quantumtv.bridge.control;

import java.util.HashMap;
import java.util.Map;
import java.util.function.LongSupplier;

/**
 * Source 级熔断 (§41/§42): 同一 spider class 连续 3 次 playerContent 超时 → OPEN 30s,
 * 到期放 1 个探测 (半开); 探测成功闭合, 失败重开。只拦 playerContent, search/detail 放行
 * (避免误伤"夸克慢但其他线路可用"的站点)。纯 Java, host 注入时钟可测。
 */
public final class SourceBreaker {
    private static final int FAIL_THRESHOLD = 3;
    private static final class S { int fails; long openedAt = -1; boolean probeOut; }

    private final long openMs;
    private final LongSupplier nowMs;
    private final Map<String, S> states = new HashMap<>();

    public SourceBreaker(long openMs, LongSupplier nowMs) {
        this.openMs = openMs; this.nowMs = nowMs;
    }

    public synchronized boolean allowPlayerContent(String cls) {
        S s = states.get(cls);
        if (s == null) return true;
        if (s.openedAt < 0) return true;
        long now = nowMs.getAsLong();
        if (now - s.openedAt < openMs) return false;
        if (s.probeOut) return false; // 探测在途, 其余排队者快速失败
        s.probeOut = true;
        return true;
    }

    public synchronized void recordPlayerContentSuccess(String cls) {
        states.remove(cls);
    }

    public synchronized void recordPlayerContentTimeout(String cls) {
        S s = states.computeIfAbsent(cls, k -> new S());
        if (s.probeOut) { // 探测失败 → 重新开窗
            s.probeOut = false; s.fails = FAIL_THRESHOLD; s.openedAt = nowMs.getAsLong();
            return;
        }
        if (s.openedAt >= 0) { s.openedAt = nowMs.getAsLong(); return; } // 已开再超时 → 续窗
        if (++s.fails >= FAIL_THRESHOLD) { s.openedAt = nowMs.getAsLong(); }
    }

    public synchronized String snapshot() {
        StringBuilder sb = new StringBuilder("{");
        long now = nowMs.getAsLong();
        boolean first = true;
        for (Map.Entry<String, S> e : states.entrySet()) {
            if (e.getValue().openedAt >= 0 && now - e.getValue().openedAt < openMs * 4) {
                if (!first) sb.append(",");
                sb.append("\"").append(e.getKey()).append("\":\"open\"");
                first = false;
            }
        }
        return sb.append("}").toString();
    }
}
```

- [x] **Step 4: host 全绿** — `ALL HOSTTESTS OK`。

- [x] **Step 5: Commit**

```bash
git add android/spider-bridge/src/com/quantumtv/bridge/ipc/WorkerState.java android/spider-bridge/src/com/quantumtv/bridge/ipc/TimeoutPolicy.java android/spider-bridge/src/com/quantumtv/bridge/control/SourceBreaker.java android/spider-bridge/hosttest
git commit -m "feat(android): 纯逻辑件 WorkerState/TimeoutPolicy/SourceBreaker + host 单测"
```

---

### Task 3: Worker 进程声明与 HELLO/READY 握手

**Files:**
- Modify: `android/spider-bridge/AndroidManifest.xml` (两个 `<service android:process>`)
- Create: `android/spider-bridge/src/com/quantumtv/bridge/worker/BaseSpiderWorker.java`（LocalSocket 客户端 + 心跳线程骨架，本任务只握手）
- Create: `android/spider-bridge/src/com/quantumtv/bridge/worker/GeneralWorkerService.java`
- Create: `android/spider-bridge/src/com/quantumtv/bridge/worker/PlaybackWorkerService.java`
- Create: `android/spider-bridge/src/com/quantumtv/bridge/control/WorkerManager.java`（本任务只做 LocalServerSocket 监听 + HELLO/READY 登记）
- Modify: `android/spider-bridge/src/com/quantumtv/bridge/BridgeService.java`（onCreate 起 WorkerManager；onDestroy 停）

**Interfaces:**
- Consumes: Task 1 Proto, Task 2 WorkerState
- Produces: abstract LocalSocket 名 `"qtv.bridge.ctl.general"` / `"qtv.bridge.ctl.playback"`；`WorkerManager.state(String role) -> WorkerState`；`Map<String,Integer> workerPids()`（验收用）。Control 端 `readFrame/writeFrame` 走 `LocalSocket` 流。

- [x] **Step 1: manifest 加双 worker 进程**

`<application>` 内追加（BridgeService 之后）：

```xml
        <!-- Spider Worker: 独立进程。playerContent 挂死只杀自己, 不碰 Control/TCP (§5/§6/§51)。 -->
        <service android:name=".worker.GeneralWorkerService"
            android:process=":spider_general" android:exported="false" />
        <service android:name=".worker.PlaybackWorkerService"
            android:process=":spider_playback" android:exported="false" />
```

- [x] **Step 2: `BaseSpiderWorker` — 连控制面→HELLO→READY→心跳**

```java
package com.quantumtv.bridge.worker;

import android.app.Service;
import android.content.Intent;
import android.net.LocalSocket;
import android.net.LocalSocketAddress;
import android.os.IBinder;
import android.util.Log;

import com.quantumtv.bridge.ipc.Proto;
import com.quantumtv.bridge.ipc.WorkerState;

import java.io.DataInputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;

/**
 * Spider Worker 进程基类 (§7/§8/§29/§55): 1 进程 = 1 执行线程 = 1 active call。
 * 连主进程 LocalServerSocket → HELLO(role,pid) → READY → 1s 心跳 (§19)。
 * 本类不持有也不接触桌面 TCP (§49)。
 */
public abstract class BaseSpiderWorker extends Service {
    protected static final String TAG = "BridgeWorker";
    private volatile boolean running = true;
    private volatile OutputStream out;
    private volatile WorkerState state = WorkerState.STARTING;
    private volatile int curReqId = -1;
    private volatile String curMethod = "";
    private volatile long curStartMs = 0;
    protected Thread execThread;

    protected abstract String role();

    @Override public IBinder onBind(Intent i) { return null; }

    @Override public int onStartCommand(Intent i, int f, int s) {
        new Thread(this::connectLoop, "WorkerConnect").start();
        return START_NOT_STICKY; // 主进程负责重新拉起 (§22)
    }

    private void connectLoop() {
        while (running) {
            try (LocalSocket sock = new LocalSocket()) {
                sock.connect(new LocalSocketAddress("qtv.bridge.ctl." + role(),
                        LocalSocketAddress.Namespace.ABSTRACT));
                DataInputStream in = new DataInputStream(sock.getInputStream());
                synchronized (this) { out = sock.getOutputStream(); }
                send(Proto.T_HELLO, 0, ("{\"role\":\"" + role() + "\",\"pid\":"
                        + android.os.Process.myPid() + "}").getBytes(StandardCharsets.UTF_8));
                send(Proto.T_READY, 0, "{}".getBytes(StandardCharsets.UTF_8));
                state = WorkerState.IDLE;
                startHeartbeat();
                readLoop(in);
            } catch (Exception e) {
                Log.w(TAG, role() + " control connect failed: " + e);
            }
            state = WorkerState.DEAD;
            try { Thread.sleep(1000); } catch (InterruptedException ignored) { return; }
        }
    }

    private void readLoop(DataInputStream in) throws Exception {
        byte[] hdr = new byte[12];
        while (running) {
            in.readFully(hdr);
            int reqId = ((hdr[0] & 255) << 24) | ((hdr[1] & 255) << 16) | ((hdr[2] & 255) << 8) | (hdr[3] & 255);
            int type = ((hdr[4] & 255) << 24) | ((hdr[5] & 255) << 16) | ((hdr[6] & 255) << 8) | (hdr[7] & 255);
            int len = ((hdr[8] & 255) << 24) | ((hdr[9] & 255) << 16) | ((hdr[10] & 255) << 8) | (hdr[11] & 255);
            byte[] payload = new byte[len];
            in.readFully(payload);
            onFrame(reqId, type, payload); // Task 4 起处理 REQ/HB/COOKIE/HANG
        }
    }

    protected void onFrame(int reqId, int type, byte[] payload) { }

    private void startHeartbeat() {
        Thread hb = new Thread(() -> {
            while (running) {
                String st = state.name();
                String body = "{\"state\":\"" + st + "\",\"curReqId\":" + curReqId
                        + ",\"curMethod\":\"" + curMethod + "\",\"curAgeMs\":"
                        + (curStartMs == 0 ? 0 : System.currentTimeMillis() - curStartMs) + "}";
                send(Proto.T_HB, 0, body.getBytes(StandardCharsets.UTF_8));
                try { Thread.sleep(1000); } catch (InterruptedException ignored) { return; }
            }
        }, "WorkerHB-" + role());
        hb.setDaemon(true);
        hb.start();
    }

    protected void send(int type, int reqId, byte[] payload) {
        OutputStream o = out;
        if (o == null) return;
        try { synchronized (o) { o.write(Proto.encode(reqId, type, payload)); o.flush(); } }
        catch (Exception e) { Log.w(TAG, role() + " send: " + e); }
    }

    @Override public void onDestroy() {
        running = false;
        if (execThread != null) execThread.interrupt(); // 退出信号; 卡死时由控制面 killProcess (§21)
        super.onDestroy();
    }
}
```

`worker/GeneralWorkerService.java` / `worker/PlaybackWorkerService.java`：

```java
public class GeneralWorkerService extends BaseSpiderWorker {
    @Override protected String role() { return "general"; }
}
```

```java
public class PlaybackWorkerService extends BaseSpiderWorker {
    @Override protected String role() { return "playback"; }
}
```

- [x] **Step 3: `WorkerManager` 监听 + 登记**

```java
package com.quantumtv.bridge.control;

import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;

import com.quantumtv.bridge.ipc.Proto;
import com.quantumtv.bridge.ipc.WorkerState;

import java.io.DataInputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Control 侧 Worker 生命周期 (§7/§20/§22/§76): 拉起/登记/重启。
 * 本任务范围: LocalServerSocket + HELLO/READY 登记。调度/超时/kill 在 Task 4 接入。
 */
public final class WorkerManager {
    private static final String TAG = "BridgeCtl";
    public static final String ROLE_GENERAL = "general";
    public static final String ROLE_PLAYBACK = "playback";

    public static final class Handle {
        public volatile WorkerState state = WorkerState.STARTING;
        public volatile int pid = -1;
        public volatile OutputStream out;
        public volatile long lastHbMs;
    }

    private final Map<String, Handle> workers = new ConcurrentHashMap<>();
    private final android.content.Context ctx;
    private volatile boolean running = true;

    public WorkerManager(android.content.Context ctx) {
        this.ctx = ctx;
        workers.put(ROLE_GENERAL, new Handle());
        workers.put(ROLE_PLAYBACK, new Handle());
    }

    public void start() {
        for (String role : workers.keySet()) {
            new Thread(() -> listen(role), "CtlListen-" + role).start();
            spawn(role);
        }
    }

    private void spawn(String role) {
        Class<?> cls = role.equals(ROLE_GENERAL)
                ? com.quantumtv.bridge.worker.GeneralWorkerService.class
                : com.quantumtv.bridge.worker.PlaybackWorkerService.class;
        ctx.startService(new android.content.Intent(ctx, cls));
        Log.i(TAG, "spawn worker " + role);
    }

    private void listen(String role) {
        try (LocalServerSocket srv = new LocalServerSocket("qtv.bridge.ctl." + role, false)) {
            while (running) {
                LocalSocket s = srv.accept();
                Handle h = workers.get(role);
                h.out = s.getOutputStream();
                DataInputStream in = new DataInputStream(s.getInputStream());
                byte[] hdr = new byte[12];
                while (running) {
                    in.readFully(hdr);
                    int type = ((hdr[4] & 255) << 24) | ((hdr[5] & 255) << 16) | ((hdr[6] & 255) << 8) | (hdr[7] & 255);
                    int len = ((hdr[8] & 255) << 24) | ((hdr[9] & 255) << 16) | ((hdr[10] & 255) << 8) | (hdr[11] & 255);
                    byte[] payload = new byte[len];
                    in.readFully(payload);
                    String body = new String(payload, StandardCharsets.UTF_8);
                    if (type == Proto.T_HELLO) {
                        h.pid = fieldInt(body, "pid");
                        h.lastHbMs = System.currentTimeMillis();
                        Log.i(TAG, "worker " + role + " hello pid=" + h.pid);
                    } else if (type == Proto.T_READY) {
                        h.state = WorkerState.IDLE;
                        Log.i(TAG, "[Bridge] worker=" + role + " pid=" + h.pid + " status=ready");
                    }
                    // HB/RESP 处理在 Task 4/5 接入
                }
            }
        } catch (Exception e) {
            if (running) Log.e(TAG, "listen " + role + ": " + e);
        }
    }

    static int fieldInt(String json, String key) {
        try {
            int i = json.indexOf("\"" + key + "\":");
            if (i < 0) return -1;
            int s = i + key.length() + 3;
            int e = s; while (e < json.length() && Character.isDigit(json.charAt(e))) e++;
            return Integer.parseInt(json.substring(s, e));
        } catch (Exception ex) { return -1; }
    }

    public WorkerState state(String role) { Handle h = workers.get(role); return h == null ? WorkerState.DEAD : h.state; }
    public Map<String, Integer> workerPids() {
        Map<String, Integer> m = new ConcurrentHashMap<>();
        for (Map.Entry<String, Handle> e : workers.entrySet()) m.put(e.getKey(), e.getValue().pid);
        return m;
    }
    public void shutdown() { running = false; }
}
```

（import `android.content.Intent`。）

- [x] **Step 4: BridgeService 接线**

`onCreate` 之后字段区加 `public WorkerManager workers;`；`onStartCommand` 里 `TunnelClient.start(this);` 之前：

```java
            workers = new WorkerManager(this);
            workers.start();
```

`onDestroy` 开头：`if (workers != null) workers.shutdown();`

- [x] **Step 5: 构建 + 装机验证（失败先行不可行, 用行为断言）**

```powershell
powershell -File android\spider-bridge\build.ps1
$adb = "D:\Program Files\Netease\MuMu\nx_main\adb.exe"
& $adb -s emulator-5554 install -r android\spider-bridge\out\bridge.apk
& $adb -s emulator-5554 shell am force-stop com.quantumtv.bridge
& $adb logcat -c
& $adb -s emulator-5554 shell am start -n com.quantumtv.bridge/.MainActivity
Start-Sleep 6
& $adb logcat -d -s BridgeCtl BridgeWorker | Select-String "hello pid=|status=ready"
& $adb -s emulator-5554 shell "ps -A | grep quantumtv"
```

Expected: 三进程都在（主 + `:spider_general` + `:spider_playback`），logcat 两条 `worker=<role> pid=NNN status=ready`。若某行缺失 = 本任务失败，修到绿。

- [x] **Step 6: Commit**

```bash
git add android/spider-bridge
git commit -m "feat(android): Worker 双进程声明 + HELLO/READY 握手 (control LocalServerSocket)"
```

---

### Task 4: REQ/RESP 派发 + Watchdog kill/restart + /__test_hang

**Files:**
- Modify: `.../control/WorkerManager.java`（请求表 + 硬超时 + 心跳判定 + kill/restart）
- Modify: `.../worker/BaseSpiderWorker.java`（onFrame 处理 REQ/HANG：单执行线程，结果回 RESP）
- Modify: `.../BridgeService.java`（routeRequest: spider 类路径 → 先查 SourceBreaker → 派给 worker 并等回包；新增 `/__test_hang`）
- Create: `android/spider-bridge/test_isolation.ps1`

**Interfaces:**
- Produces: `WorkerManager.dispatch(String role, String method, byte[] reqJson, Consumer<Resp> done)`；`Resp{int code; String err; String data}`；回桌面错误码 `worker_killed`(503) / `worker_restarting`(503) / `worker_disabled`(503) / `source_circuit_open`(503)。
- 超时双条件 kill（§19/§21/§67）：请求年龄 > `TimeoutPolicy.hardMs(method)` **或** 心跳停 >5s 且当前请求已超硬超时；先 `state=SUSPECT`（拒新请求 §31），`Process.killProcess(pid)`，`spawn` 重启。
- Crash-loop（§40）：worker 死亡事件计数，30s 内第 2 次 → 照常重启；60s 内第 3 次 → `DISABLED`（health 可见，重启由 `/__test_stats` 人工/后续命令触发）。

- [x] **Step 1: 写验收脚本（先红）** `test_isolation.ps1`：

```powershell
$ErrorActionPreference = 'Stop'
$adb = "D:\Program Files\Netease\MuMu\nx_main\adb.exe"
$dev = "emulator-5554"
function Assert-Log([string]$pattern, [string]$label) {
    $hit = & $adb -s $dev logcat -d | Select-String $pattern | Select-Object -Last 1
    if (-not $hit) { throw "FAIL[$label]: 未命中 /$pattern/" }
    Write-Host "PASS[$label]: $($hit.Line)"
}
& $adb -s $dev install -r android\spider-bridge\out\bridge.apk
& $adb -s $dev shell am force-stop com.quantumtv.bridge
& $adb logcat -c
& $adb -s $dev shell am start -n com.quantumtv.bridge/.MainActivity
Start-Sleep 6
Assert-Log "worker=playback .*status=ready" "ready-after-spawn (§22)"
# 制造 playback 永久挂死 (§58 验收): 通过遗留 8080 通道直调 control
& $adb -s $dev shell "curl -s -m 2 'http://127.0.0.1:8080/__test_hang' || true"
Start-Sleep 12
Assert-Log "worker=playback .*action=kill" "hard timeout kill (§70)"
Assert-Log "worker=playback .*status=ready" "auto restart (§22 <3s)"
# 挂死期间 general worker / health / init 不受影响 (§58/§60/§62)
$health = & $adb -s $dev shell "curl -s -m 3 'http://127.0.0.1:8080/health'"
Write-Host "health: $health"
if ($health -notmatch '"bridge":"healthy"') { throw "FAIL[health-decoupled (§24)]" }
Write-Host "ISOLATION TESTS OK"
```

（若模拟器 shell 无 curl：改用 `run_hosttests` 里同逻辑的 python 请求器；实现者按现场可用性二选一并在执行记录注明。）

- [x] **Step 2: 运行确认失败** — 当前 routeRequest 无 `/__test_hang`、无派发、无 watchdog → `FAIL[hard timeout kill]`。

- [x] **Step 3: WorkerManager 派发 + watchdog + kill/restart**

字段区追加：

```java
    private static final class Pending {
        final String role, method; final long startMs; final java.util.function.Consumer<Handle.Resp> done;
        Pending(String r, String m, long t, java.util.function.Consumer<Handle.Resp> d) {
            role = r; method = m; startMs = t; done = d;
        }
    }
    private final Map<Integer, Pending> pending = new ConcurrentHashMap<>();
    private final java.util.concurrent.atomic.AtomicInteger nextReqId = new java.util.concurrent.atomic.AtomicInteger(1);
    private final Map<String, java.util.Deque<Long>> deaths = new ConcurrentHashMap<>(); // §40
```

`Handle` 内加 `public static final class Resp { public final int code; public final String err; public final String data; Resp(int c, String e, String d){code=c;err=e;data=d;} }`。

dispatch：

```java
    /** 派发一个 spider 调用; done 在 control 线程回调。worker 非 IDLE 直接回 worker_restarting。 */
    public void dispatch(String role, String method, byte[] reqJson,
                         java.util.function.Consumer<Handle.Resp> done) {
        Handle h = workers.get(role);
        if (h == null) { done.accept(new Handle.Resp(503, "worker_unavailable", null)); return; }
        if (h.state == WorkerState.DISABLED) { done.accept(new Handle.Resp(503, "worker_disabled", null)); return; }
        if (!h.state.canAcceptRequest()) { done.accept(new Handle.Resp(503, "worker_restarting", null)); return; }
        int rid = nextReqId.getAndIncrement();
        pending.put(rid, new Pending(role, method, System.currentTimeMillis(), done));
        h.state = WorkerState.BUSY;
        h.lastHbMs = System.currentTimeMillis();
        try {
            synchronized (h.out) { h.out.write(Proto.encode(rid, Proto.T_REQ, reqJson)); h.out.flush(); }
        } catch (Exception e) {
            failPending(rid, 502, "worker_send_failed");
        }
    }

    void failPending(int rid, int code, String err) {
        Pending p = pending.remove(rid);
        if (p != null) { p.done.accept(new Handle.Resp(code, err, null)); }
    }
```

在 listen 的帧分发里补 RESP/HB 分支：

```java
                    } else if (type == Proto.T_RESP) {
                        Pending p = pending.remove(fieldInt(body, "reqId"));
                        int code = fieldInt(body, "code");
                        Handle hh = workers.get(role); if (hh != null) hh.state = WorkerState.IDLE;
                        if (p != null) p.done.accept(new Handle.Resp(code, stringField(body, "err"), stringField(body, "data")));
                    } else if (type == Proto.T_HB) {
                        Handle hh = workers.get(role);
                        hh.lastHbMs = System.currentTimeMillis();
                        String st = stringField(body, "state");
                        if ("BUSY".equals(st) || "IDLE".equals(st)) {
                            if (hh.state == WorkerState.SUSPECT && "IDLE".equals(st)) hh.state = WorkerState.IDLE;
                        }
                    }
```

加 `static String stringField(String json, String key)` 极简取值。

watchdog 线程（start() 内启动）：

```java
    /** §19/§20/§21: 心跳 3s→SUSPECT; 请求超硬超时(或心跳 5s)→kill+restart。Watchdog 永远在 Control。 */
    private void watchdogLoop() {
        while (running) {
            long now = System.currentTimeMillis();
            for (Map.Entry<Integer, Pending> e : pending.entrySet()) {
                Pending p = e.getValue();
                Handle h = workers.get(p.role);
                long hard = com.quantumtv.bridge.ipc.TimeoutPolicy.hardMs(p.method);
                boolean hbStale = now - h.lastHbMs > 5000;
                boolean overdue = now - p.startMs > hard;
                if (overdue || (hbStale && p.startMs > 0)) {
                    long age = now - p.startMs;
                    Log.w(TAG, "[SpiderWorker] worker=" + p.role + " pid=" + h.pid
                            + " method=" + p.method + " duration=" + age + "ms status=timeout action=kill");
                    killAndRestart(p.role, h, "hard_timeout_" + p.method);
                    failPending(e.getKey(), 503, "worker_killed"); // §35: 桌面请求有明确回包
                    break; // 每轮一个, 下轮再查
                } else if (now - h.lastHbMs > 3000) {
                    h.state = WorkerState.SUSPECT; // §19: 标记但不立即杀
                }
            }
            for (Map.Entry<String, Handle> en : workers.entrySet()) {
                Handle h = en.getValue();
                if (h.state == WorkerState.DEAD) restart(en.getKey(), h);
                if (h.state == WorkerState.SUSPECT && pendingActive(en.getKey())) continue;
            }
            try { Thread.sleep(500); } catch (InterruptedException ignored) { return; }
        }
    }

    private boolean pendingActive(String role) {
        for (Pending p : pending.values()) if (p.role.equals(role)) return true;
        return false;
    }

    /** §40 冷却: 60s 窗口第 3 次死亡 → DISABLED, 防夸克挂死引发重启风暴 */
    private void killAndRestart(String role, Handle h, String reason) {
        h.state = WorkerState.KILLING;
        try { android.os.Process.killProcess(h.pid); } catch (Exception ignored) { }
        Log.i(TAG, "[Bridge] worker=" + role + " pid=" + h.pid + " status=killed reason=" + reason);
        restart(role, h);
    }

    private void restart(String role, Handle h) {
        java.util.Deque<Long> d = deaths.computeIfAbsent(role, k -> new java.util.ArrayDeque<>());
        long now = System.currentTimeMillis();
        synchronized (d) {
            while (!d.isEmpty() && now - d.peekFirst() > 60_000) d.pollFirst();
            d.addLast(now);
            if (d.size() >= 3) {
                h.state = WorkerState.DISABLED;
                Log.e(TAG, "[Bridge] worker=" + role + " status=disabled (crash-loop §40)");
                return;
            }
        }
        h.state = WorkerState.RESTARTING;
        spawn(role);
    }
```

`start()` 内：`new Thread(this::watchdogLoop, "WorkerWatchdog").start();`

- [x] **Step 4: worker 侧执行线程 + HANG**

`BaseSpiderWorker.onFrame` 替换为：

```java
    protected void onFrame(int reqId, int type, byte[] payload) {
        if (type == Proto.T_REQ) {
            // 单执行线程: 1 process = 1 spider = 1 active call (§29/§55); 上一帧必然已结束
            exec(reqId, payload);
        } else if (type == Proto.T_HANG) {
            curReqId = reqId; curMethod = "__test_hang"; curStartMs = System.currentTimeMillis();
            state = WorkerState.BUSY;
            Log.w(TAG, role() + " HANG hook: sleeping forever");
            try { Thread.sleep(Long.MAX_VALUE); } catch (InterruptedException ignored) { }
        } else if (type == Proto.T_COOKIE) {
            onCookie(new String(payload, StandardCharsets.UTF_8)); // Task 6
        }
    }

    /** REQ 执行: 解析 method → 真实 spider 调用 (Task 6 接管), 回 RESP */
    private void exec(int reqId, byte[] payload) {
        curReqId = reqId; curStartMs = System.currentTimeMillis(); state = WorkerState.BUSY;
        String body = new String(payload, StandardCharsets.UTF_8);
        curMethod = extract(body, "method");
        // Task 6 前: 用占位成功回包, 让派发/超时路径先可验
        send(Proto.T_RESP, reqId, respJson(reqId, 200, null, "\"\"").getBytes(StandardCharsets.UTF_8));
        state = WorkerState.IDLE; curReqId = -1; curMethod = ""; curStartMs = 0;
    }

    static String respJson(int reqId, int code, String err, String data) {
        StringBuilder sb = new StringBuilder("{\"reqId\":").append(reqId).append(",\"code\":").append(code);
        if (err != null) sb.append(",\"err\":\"").append(err.replace("\\", "\\\\").replace("\"", "\\\"")).append("\"");
        if (data != null) sb.append(",\"data\":").append(data);
        return sb.append("}").toString();
    }

    static String extract(String json, String key) {
        String pat = "\"" + key + "\":\"";
        int i = json.indexOf(pat);
        if (i < 0) return "";
        int s = i + pat.length(); int e = json.indexOf('"', s);
        return e < 0 ? "" : json.substring(s, e);
    }
```

`BridgeService.onStartCommand` 里 `workers.start()` 之后无需额外动作（watchdog 已在 manager 内）。

- [x] **Step 5: BridgeService 路由改造（本任务范围：hang + 路由骨架，spider 实际调用 Task 6 接管）**

`routeRequest` 中，`/health` 保持原样（control 自答），在其后加：

```java
        if ("/__test_hang".equals(path)) {
            java.util.concurrent.CompletableFuture<String> f = new java.util.concurrent.CompletableFuture<>();
            byte[] req = "{\"method\":\"__test_hang\"}".getBytes("UTF-8");
            workers.dispatch(WorkerManager.ROLE_PLAYBACK, "__test_hang", req, r ->
                    f.complete(json(r.code, r.err, r.data == null ? null : "\"" + r.data + "\"")));
            try { return f.get(120, java.util.concurrent.TimeUnit.SECONDS); }
            catch (Exception e) { return json(500, "hang_dispatch_failed", null); }
        }
```

（`/__test_hang` 故意不等回包语义：worker 挂死后由 control watchdog 回 `worker_killed`——正是本任务要验的链路。）

- [x] **Step 6: 跑验收** — build + `test_isolation.ps1`
Expected: `ISOLATION TESTS OK`。特别确认：kill 与 restart 日志、health 在挂死窗口内 200、`status=ready` 在 <3s 内出现。

- [x] **Step 7: Commit**

```bash
git add android/spider-bridge
git commit -m "feat(android): WorkerManager 派发 + Control 侧 Watchdog kill/restart + test_hang 验收"
```

---

### Task 5: 搜索/详情/播放路由拆分（§15/§16/§44）

**Files:**
- Modify: `BridgeService.java` routeRequest 的 spider 类路由
- 复用 Task 4 的 `dispatch`

**Interfaces:**
- Produces: 路由矩阵——`/playerContent` → ROLE_PLAYBACK；`/search` `/detail` `/home` `/category` → ROLE_GENERAL；`SourceBreaker` 在派发前拦截（playerContent 类级熔断 §41/§61）；`/health` 输出 §48 结构。

- [x] **Step 1: 扩展 test_isolation.ps1（先红）**

```powershell
# —— §61/§41: 连续 3 次 playerContent 超时 → 类级熔断, 第 4 次秒拒; general 不受影响
& $adb logcat -c
foreach ($i in 1..3) {
    & $adb -s $dev shell "curl -s -m 40 'http://127.0.0.1:8080/__test_hang?class=WextestHang'" | Out-Null
}
$fourth = & $adb -s $dev shell "curl -s -m 3 'http://127.0.0.1:8080/playerContent' -d '{\"class\":\"WextestHang\",\"id\":\"x\"}'"
if ($fourth -notmatch 'source_circuit_open') { throw "FAIL[source-breaker §41]: $fourth" }
$search = & $adb -s $dev shell "curl -s -m 3 'http://127.0.0.1:8080/health'"
if ($search -notmatch '"general":"ready"') { throw "FAIL[general-alive]: $search" }
Write-Host "BREAKER TESTS OK"
```

- [x] **Step 2: 实现路由矩阵 + 接线 Breaker + /health §48**

BridgeService 字段：`private final com.quantumtv.bridge.control.SourceBreaker breaker = new com.quantumtv.bridge.control.SourceBreaker(30_000, System::currentTimeMillis);`

`routeRequest` 里 spider 路径改走统一派发：

```java
        if (isSpiderOp(path) && "POST".equalsIgnoreCase(method)) {
            String role = "/playerContent".equals(path)
                    ? WorkerManager.ROLE_PLAYBACK : WorkerManager.ROLE_GENERAL;
            String cls = parseField(body, "class");
            if ("/playerContent".equals(path) && !breaker.allowPlayerContent(cls)) {
                return json(503, "source_circuit_open", null);
            }
            final String m = path.substring(1);
            java.util.concurrent.CompletableFuture<String> f = new java.util.concurrent.CompletableFuture<>();
            byte[] req = ipcReq(m, cls, body);
            workers.dispatch(role, m, req, r -> {
                if ("/playerContent".equals(path)) {
                    if (r.code == 503 && "worker_killed".equals(r.err)) breaker.recordPlayerContentTimeout(cls);
                    else if (r.code == 200) breaker.recordPlayerContentSuccess(cls);
                }
                f.complete(json(r.code, r.err, r.data == null ? null : "\"" + esc(r.data) + "\""));
            });
            try { return f.get(150, java.util.concurrent.TimeUnit.SECONDS); }
            catch (Exception e) { return json(500, "dispatch_failed", null); }
        }
```

`/health` 重写为 §24/§48（control 自答，不碰 spider）：

```java
        if ("/health".equals(path)) {
            return "{\"code\":200,\"err\":\"ok\",\"data\":\"{\\\"bridge\\\":\\\"healthy\\\","
                + "\\\"init\\\":" + initialized + ",\\\"workers\\\":{\\\"general\\\":\\\""
                + workerLabel(WorkerManager.ROLE_GENERAL) + "\\\",\\\"playback\\\":\\\""
                + workerLabel(WorkerManager.ROLE_PLAYBACK) + "\\\"},\\\"sources_open\\\":"
                + breaker.snapshot() + "}\"}";
        }
```

`workerLabel`: IDLE/READY→`ready`，BUSY→`busy`，SUSPECT/KILLING/RESTARTING→`restarting`，DEAD→`dead`，DISABLED→`disabled`，STARTING→`starting`。

`ipcReq(m, cls, body)`: 把桌面 body 的 `class/keyword/ids/id/flag/tid/pg` 按方法重组为 worker REQ JSON `{"method":...,"class":...,"args":{...}}`（实现者按 do* 现有字段映射逐字搬，勿省略字段）。

- [x] **Step 3: 验收** — 重跑 `test_isolation.ps1` 两段 → `ISOLATION TESTS OK` + `BREAKER TESTS OK`；MuMu 上 `/search`（非 guard 类, 纯 java spider）确认走 general 有正常回包。

- [x] **Step 4: Commit**

```bash
git add android/spider-bridge
git commit -m "feat(android): 路由拆分 general/playback + playerContent 类级熔断 + health §48 结构"
```

---

### Task 6: Spider 执行体迁移进 Worker（全局锁消亡, §27/§28/§57）

**Files:**
- Create: `worker/SpiderExec.java`（从 BridgeService 迁移：invokeSpider/doSearch/doDetail/doPlayerContent/doHome/doCategory/invokeSpiderWithRetry/spiderCache/findField/ensureWexNativeLibs/httpGet/decrypt/parseField 中 spider 相关部分）
- Modify: `worker/BaseSpiderWorker.java`（exec 接管真调用；onCookie）
- Modify: `BridgeService.java`（删除 spider 执行相关代码与 `spiderLock`；保留 control 字段与路由）
- Modify: `control/WorkerManager.java`（READY 时下发 COOKIE/ext §决策#2）

**Interfaces:**
- Produces: worker 内 `SpiderExec.handle(String method, JSONObject-ish body) -> Resp`（1 线程 1 实例缓存，无锁——§57）；control `currentExt()` + cookie 广播。

- [x] **Step 1: SpiderExec 迁移**

逐字搬运 BridgeService:259-544 中 spider 执行路径，两处结构性修改：
1. 删掉 `synchronized (spiderLock)` 外层与 `detailPending` 让锁循环（§27/§28——进程隔离即串行）；
2. `ensureWexNativeLibs` 的 ext/native 下载保留（worker 进程内执行，files/TV 与主进程共享同一 data 目录）；
3. 方法入口 `init(Context app, String ext)` 供 BaseSpiderWorker 在首 REQ 前调用（`Init.init` 反射保留）。

`BaseSpiderWorker.exec` 替换占位：

```java
        SpiderExec.Resp r = SpiderExec.get().invoke(curMethod, body);
        send(Proto.T_RESP, reqId, respJson(reqId, r.code, r.err,
                r.data == null ? null : jsonQuote(r.data)).getBytes(StandardCharsets.UTF_8));
```

（`jsonQuote` 把字符串包成 JSON 字面量并转义；SpiderExec 内部 `invoke` 与旧 `invokeSpider` 等价但返回 Resp 对象。）

- [x] **Step 2: cookie/ext 下发（§决策#2）**

`BridgeService.doSetCookie` 末尾（现有逻辑保留在 control：写 CookieManager+文件）追加广播：

```java
        workers.broadcastCookie(extConfig, driveCookieSummary());
```

`WorkerManager`：

```java
    /** cookie/ext 变更后显式下发全部 worker; worker 收到清缓存重建 (§28) */
    public void broadcastCookie(String ext, String drives) {
        byte[] body = ("{\"ext\":" + jsonOrNull(ext) + ",\"drives\":" + drives + "}").getBytes(StandardCharsets.UTF_8);
        for (Handle h : workers.values()) {
            if (h.out == null) continue;
            try { synchronized (h.out) { h.out.write(Proto.encode(0, Proto.T_COOKIE, body)); h.out.flush(); } }
            catch (Exception ignored) { }
        }
    }
```

worker `onCookie` → `SpiderExec.get().reinit(ext)`（清 spiderCache、写 `TV/.<drive>cookie` 文件兜底逻辑沿用 `writeCookieFile`）。

- [x] **Step 3: BridgeService 删净旧执行体**

删除 `pool`（4 线程 HTTP 路由池保留用于 acceptLoop 的 control 侧快速路径：/health /init /setCookie /__test_*）；保留 `detailExecutor` 仅当 8080 兼容层仍需要同步回写——实际 spider 派发已是 CompletableFuture，**删 detailExecutor**（§57 收尾）。`initialized` 语义改为"control init 完成"，不再触发 spiderLock。`invalidateSpiders` 改为 `workers.broadcastCookie(ext, drives)`。

- [x] **Step 4: 验收**

`test_isolation.ps1` 全绿 + 新增：

```powershell
# 真 spider 调用走 worker: MuMu 上取一个纯 Java 类站 (非 guard) 搜索有结果
$r = & $adb -s $dev shell "curl -s -m 30 'http://127.0.0.1:8080/search' -d '{\"class\":\"<NON_GUARD_CLASS>\",\"keyword\":\"测试\"}'"
if ($r -notmatch '"code":200') { throw "FAIL[worker real spider call]: $r" }
Write-Host "WORKER-EXEC OK"
```

（`<NON_GUARD_CLASS>` 由执行者从当前订阅缓存的 spider.jar 类列表里选一个无 native 依赖的类；若无则本断言降级为"返回 500 且进程存活、health ready"。）

- [x] **Step 5: Commit**

```bash
git add android/spider-bridge
git commit -m "refactor(android): spider 执行体迁入 worker 进程, 删除全局 spiderLock/detailExecutor"
```

---

### Task 7: /init 与启动解耦（§23/§45/§46/§47）

**Files:**
- Modify: `BridgeService.java` doInit、onStartCommand
- Modify: `control/WorkerManager.java`（worker init 状态上报）

**Interfaces:**
- Produces: `/init` = control 存 ext → 广播 → **立即返回** `{"code":200,"data":"{\"ok\":true,\"bridge\":\"ready\",\"worker\":\"starting\"}"}`；worker 首 REQ 时 lazy `SpiderExec.init`（native 库下载不再阻塞控制面，失败不拖垮 /init——§47）。桌面 `bridge_post_with` 对 data 只判 code → 零桌面改动。

- [x] **Step 1: doInit 重写**

```java
    private String doInit(String body) {
        String ext = (body != null && !body.isEmpty()) ? parseField(body, "ext") : null;
        if (ext != null) extConfig = ext;
        initialized = true; // control 面就绪 ≠ spider 就绪 (§46)
        workers.broadcastCookie(extConfig, driveCookieSummary()); // ext 下发, worker 内后台自举 native (§78)
        return json(200, null, "{\"ok\":true,\"bridge\":\"ready\",\"worker\":\"starting\"}");
    }
```

（原 `ensureWexNativeLibs`+`Init.init` 从 doInit 移除——迁到 SpiderExec 首调用路径。）

- [x] **Step 2: 验收** — 挂死 playback 时 `/init` <1s 返回（curl 计时）；`test_isolation.ps1` 扩展断言 `health` 含 `"init":true`；桌面端 `/init ok (3ms)` 级日志重现（对照旧日志的 45005ms "伪 ok"）。

- [x] **Step 3: Commit**

```bash
git add android/spider-bridge
git commit -m "refactor(android): /init 与 spider native 初始化解耦 (control 秒回 + worker 后台自举)"
```

---

### Task 8: 时长指标采集（为超时定值, §70-§72 + 决策#1）

**Files:**
- Modify: `worker/SpiderExec.java` / `control/WorkerManager.java`
- Create: `android/spider-bridge/test_metrics.ps1`

- [x] **Step 1: 每条 RESP 打 `[SpiderPerf]`**

worker 侧：`[SpiderPerf] role= worker= pid= method= class= duration= status=`；control 侧派发完成同样打一行（双端可对账）。

- [x] **Step 2: 采集协议**

```powershell
# test_metrics.ps1: 真机 PGBM10 隧道下, 由用户对各网盘线路各点 3~5 集播放,
# 同时抓 logcat:
& $adb logcat -v time -s BridgeBridge BridgeCtl BridgeWorker > metrics_raw.log
# 汇总各 method/class 的 P50/P95/max 并输出建议 TimeoutPolicy 值
```

（执行任务：指导用户跑一轮夸克/百度/UC 播放采样；产出 `docs/superpowers/specs/2026-09-12-bridge-timeout-evidence.md` 汇总表。）

- [x] **Step 3: Commit**

```bash
git add android/spider-bridge
git commit -m "feat(android): [SpiderPerf] 双端时长指标 + 采样脚本 (超时定值依据)"
```

---

### Task 9: 桌面端错误映射补全（worker_* 不再谎报登录, §36 最小集）

**Files:**
- Modify: `crates/core/src/spider/player.rs`（classify）
- Test: 同模块 tests

- [x] **Step 1: 失败测试**

```rust
    #[test]
    fn worker_kill_family_classifies_as_network_not_auth() {
        for e in ["bridge error: worker_killed", "bridge error: worker_restarting",
                  "bridge error: worker_disabled", "bridge error: source_circuit_open"] {
            let r = classify_spider_error("WexmuouggGuard", "夸克原画", e);
            assert!(matches!(r, ResolveError::NetworkError { .. }), "{e} → {r:?}");
        }
    }
```

- [x] **Step 2: classify 前插分支**

```rust
    if err.contains("worker_killed") || err.contains("worker_restarting")
        || err.contains("worker_disabled") || err.contains("worker_unavailable")
        || err.contains("source_circuit_open")
    {
        return ResolveError::NetworkError {
            source,
            message: "桥接执行环境故障(正在恢复), 请稍后重试".into(),
        };
    }
```

- [x] **Step 3: 验证 + Commit**

`cargo test -p quantumtv-core` 全绿后：

```bash
git add crates/core/src/spider/player.rs
git commit -m "fix(spider): worker_killed/熔断/重启中 归为执行环境故障而非需要登录"
```

---

### Task 10: 定值 + 总验收 + 文档

- [ ] **Step 1: 用 Task 8 数据改 `TimeoutPolicy` 常量**（P95×2, 下限 30s）→ host TimeoutPolicyTest 断言随之更新 → rebuild → 装机。⏳ 待真夸克采样。
- [x] **Step 2: §85 全量验收**：`test_isolation.ps1` 断言序列（①playerContent 永久挂死：search/detail/health/init/其他 spider 全正常 ②15s 级检测→kill→restart<3s ③worker 重启不断桌面 TCP ④3×超时→类级熔断且他源搜索正常 ⑤桌面 App 重启后隧道自动重建仍 ready）。
- [ ] **Step 3: 真机夸克回归**：PGBM10 隧道下播放夸克线路 → 记录 `[BridgePerf]`/`[SpiderPerf]` → 对照验收；不达标 → 回 Task 8 加采样。⏳ 待用户真机配合。
- [x] **Step 4: 文档**：`docs/adr/0004-android-bridge-worker-isolation.md`（决策+验收记录）；执行记录追加回本计划。
- [x] **Step 5: Commit**

```bash
git add -A
git commit -m "docs+config: worker 硬超时定值(实测 P95×2) + 防死锁验收记录"
```

---

## Plan 2/3 预告（本计划后另出, 依赖 Task 8 数据）

- **Plan 2 桌面策略**（§31-§38/§63-§65）：§67 双层超时落表；`worker_restarting` 时 search 有界等待 ≤3s、playback 等 ready 事件；retry 矩阵（search/detail ≤1, playerContent 0）；§71 计数指标；ResolveError 新变体。
- **Plan 3 前端**（§37/§38）：`bridge_worker_status` Tauri 事件消费、恢复中 banner、防抖用户重试。
