package com.quantumtv.bridge.control;

import java.util.HashMap;
import java.util.Map;
import java.util.function.LongSupplier;

/**
 * Source 级熔断 (方案 §41/§42): 同一 spider class 连续 3 次 playerContent 超时 → OPEN 30s,
 * 到期放 1 个探测 (半开); 探测成功闭合, 失败重开。只拦 playerContent, search/detail 放行
 * (避免误伤"夸克慢但同站其他线路仍可用")。纯 Java, host 注入时钟可测。
 */
public final class SourceBreaker {
    private static final int FAIL_THRESHOLD = 3;

    private static final class S {
        int fails;
        long openedAt = -1;
        boolean probeOut;
    }

    private final long openMs;
    private final LongSupplier nowMs;
    private final Map<String, S> states = new HashMap<>();

    public SourceBreaker(long openMs, LongSupplier nowMs) {
        this.openMs = openMs;
        this.nowMs = nowMs;
    }

    /** true = 允许向 worker 派发该 class 的 playerContent (closed, 或半开探测资格) */
    public synchronized boolean allowPlayerContent(String cls) {
        S s = states.get(cls);
        if (s == null) return true;
        if (s.openedAt < 0) return true;
        long now = nowMs.getAsLong();
        if (now - s.openedAt < openMs) return false;
        if (s.probeOut) return false; // 探测在途, 其余秒拒
        s.probeOut = true;
        return true;
    }

    public synchronized void recordPlayerContentSuccess(String cls) {
        states.remove(cls);
    }

    public synchronized void recordPlayerContentTimeout(String cls) {
        S s = states.computeIfAbsent(cls, k -> new S());
        if (s.probeOut) { // 半开探测失败 → 重新开窗
            s.probeOut = false;
            s.fails = FAIL_THRESHOLD;
            s.openedAt = nowMs.getAsLong();
            return;
        }
        if (s.openedAt >= 0) { // 已开又超时 → 续窗
            s.openedAt = nowMs.getAsLong();
            return;
        }
        if (++s.fails >= FAIL_THRESHOLD) {
            s.openedAt = nowMs.getAsLong();
        }
    }

    /** /health 展示用: {"classA":"open",...} (仅统计未远离开窗的项) */
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
