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
        t(!b.allowPlayerContent("WexquarkGuard"), "3rd → open (§42)");
        t(b.allowPlayerContent("WexotherGuard"), "other source unaffected (§41)");
        clock[0] = 29_000;
        t(!b.allowPlayerContent("WexquarkGuard"), "still open at 29s");
        clock[0] = 31_000;
        t(b.allowPlayerContent("WexquarkGuard"), "half-open: 1 probe allowed");
        t(!b.allowPlayerContent("WexquarkGuard"), "only one probe in flight");
        b.recordPlayerContentTimeout("WexquarkGuard"); // 探测失败 → 重开
        t(!b.allowPlayerContent("WexquarkGuard"), "reopen after failed probe");
        clock[0] = 70_000;
        t(b.allowPlayerContent("WexquarkGuard"), "second window half-open");
        b.recordPlayerContentSuccess("WexquarkGuard"); // 探测成功 → 闭合
        t(b.allowPlayerContent("WexquarkGuard"), "closed after success");
        t(b.snapshot().startsWith("{"), "snapshot is json object");
        System.out.println("SourceBreakerTest OK");
    }
}
