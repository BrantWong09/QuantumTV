import com.quantumtv.bridge.ipc.WorkerState;

public class WorkerStateTest {
    static void t(boolean c, String m) { if (!c) throw new AssertionError(m); }
    public static void main(String[] a) {
        t(WorkerState.IDLE.canAcceptRequest(), "idle accepts");
        t(!WorkerState.BUSY.canAcceptRequest(), "busy rejects");
        t(!WorkerState.STARTING.canAcceptRequest(), "starting rejects");
        t(!WorkerState.SUSPECT.canAcceptRequest(), "suspect rejects (§31: 标记后停发新请求)");
        t(!WorkerState.KILLING.canAcceptRequest(), "killing rejects");
        t(!WorkerState.DEAD.canAcceptRequest(), "dead rejects");
        t(!WorkerState.RESTARTING.canAcceptRequest(), "restarting rejects");
        t(!WorkerState.DISABLED.canAcceptRequest(), "disabled rejects (§40 crash-loop)");
        System.out.println("WorkerStateTest OK");
    }
}
