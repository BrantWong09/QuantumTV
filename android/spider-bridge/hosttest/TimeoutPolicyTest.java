import com.quantumtv.bridge.ipc.TimeoutPolicy;

public class TimeoutPolicyTest {
    static void t(boolean c, String m) { if (!c) throw new AssertionError(m); }
    public static void main(String[] a) {
        // 阶段 A 宽容值: 等于旧桌面容忍度, 只防永久挂死 (§74: 不得反向加大当解药)
        t(TimeoutPolicy.hardMs("playerContent") == 90_000L, "pc 90s phase-A");
        t(TimeoutPolicy.hardMs("search") == 60_000L, "search 60s phase-A");
        t(TimeoutPolicy.hardMs("detail") == 60_000L, "detail 60s phase-A");
        t(TimeoutPolicy.hardMs("init") == 30_000L, "init 30s phase-A");
        t(TimeoutPolicy.hardMs("__test_hang") == 10_000L, "test-hang 10s");
        t(TimeoutPolicy.hardMs("unknown") == 60_000L, "default 60s");
        System.out.println("TimeoutPolicyTest OK");
    }
}
