package com.quantumtv.bridge.ipc;

/**
 * Worker 硬超时表 (方案 §9/§67/§69): 含义不是取消线程, 而是 Control Watchdog 杀进程的依据。
 * 阶段 A 宽容值 = 旧桌面容忍度, 只防永久挂死、不引入新失败; Task 8 采集真实分布后调低
 * (原则: 实测 P95 × 2, 下限 30s)。§74: 禁止反向加大当解药。volatile 便于热调与 __test_stats 展示。
 */
public final class TimeoutPolicy {
    private TimeoutPolicy() {}

    public static volatile long PLAYERCONTENT_MS = 90_000;
    public static volatile long SEARCH_MS = 60_000;
    public static volatile long DETAIL_MS = 60_000;
    public static volatile long INIT_MS = 30_000;
    /** 隔离验收钩子专用短超时, 让 test_isolation.ps1 秒级完成 (仅 debuggable APK 可触发) */
    public static volatile long TEST_HANG_MS = 10_000;

    public static long hardMs(String method) {
        switch (method) {
            case "playerContent": return PLAYERCONTENT_MS;
            case "search": return SEARCH_MS;
            case "detail": case "home": case "category": return DETAIL_MS;
            case "init": return INIT_MS;
            case "__test_hang": return TEST_HANG_MS;
            default: return 60_000;
        }
    }
}
