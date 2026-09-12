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
 * Spider Worker 进程基类 (方案 §7/§8/§29/§55): 1 进程 = 1 执行线程 = 1 active call。
 * 连主进程 LocalServerSocket → HELLO(role,pid) → READY → 1s 心跳 (§19)。
 * 本类不持有也不接触桌面 TCP (§49)。REQ 真实执行在 Task 6 接管。
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

    @Override public void onCreate() {
        super.onCreate();
        // worker 进程独立 WebView CookieManager (§决策#2): 登录后由控制面 T_COOKIE 显式下发
        SpiderExec.get().attach(getApplicationContext(), "");
    }

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
                out = sock.getOutputStream();
                send(Proto.T_HELLO, 0, ("{\"role\":\"" + role() + "\",\"pid\":"
                        + android.os.Process.myPid() + "}").getBytes(StandardCharsets.UTF_8));
                send(Proto.T_READY, 0, "{}".getBytes(StandardCharsets.UTF_8));
                state = WorkerState.IDLE;
                startHeartbeat();
                readLoop(in);
            } catch (Exception e) {
                Log.d(TAG, role() + " control connect failed: " + e);
            }
            state = WorkerState.DEAD;
            out = null;
            try { Thread.sleep(1000); } catch (InterruptedException ignored) { return; }
        }
    }

    private void readLoop(DataInputStream in) throws Exception {
        byte[] hdr = new byte[12];
        while (running) {
            in.readFully(hdr);
            int reqId = le32(hdr, 0);
            int type = le32(hdr, 4);
            int len = le32(hdr, 8);
            if (len < 0 || len > Proto.MAX_PAYLOAD) break;
            byte[] payload = new byte[len];
            in.readFully(payload);
            onFrame(reqId, type, payload); // Task 4 起处理 REQ/HB/COOKIE/HANG
        }
    }

    private static int le32(byte[] b, int o) {
        return ((b[o] & 255) << 24) | ((b[o + 1] & 255) << 16) | ((b[o + 2] & 255) << 8) | (b[o + 3] & 255);
    }

    protected void onFrame(int reqId, int type, byte[] payload) {
        if (type == Proto.T_REQ) {
            // 单执行线程: 1 process = 1 spider = 1 active call (§29/§55); 上一帧必然已结束
            exec(reqId, new String(payload, StandardCharsets.UTF_8));
        } else if (type == Proto.T_HANG) {
            curReqId = reqId; curMethod = "__test_hang"; curStartMs = System.currentTimeMillis();
            state = WorkerState.BUSY;
            Log.w(TAG, role() + " HANG hook: sleeping forever");
            try { Thread.sleep(Long.MAX_VALUE); } catch (InterruptedException ignored) { }
        } else if (type == Proto.T_COOKIE) {
            onCookie(new String(payload, StandardCharsets.UTF_8)); // Task 6
        }
    }

    /** REQ 执行 (T6): 真实 spider 调用在本进程单线程内串行, 卡死由 Control Watchdog kill (§21)。 */
    private void exec(int reqId, String body) {
        curReqId = reqId; curStartMs = System.currentTimeMillis(); state = WorkerState.BUSY;
        curMethod = com.quantumtv.bridge.ipc.JsonLite.string(body, "method");
        if ("__test_hang".equals(curMethod)) {
            // 隔离验收 (§58): 模拟 native playerContent 永久挂死——不发 RESP, 等 control kill
            Log.w(TAG, role() + " HANG hook (REQ " + reqId + "): sleeping forever");
            try { Thread.sleep(Long.MAX_VALUE); } catch (InterruptedException ignored) { }
            return;
        }
        SpiderExec.Resp r = SpiderExec.get().invoke(curMethod, body);
        // §70/§72: 成功/失败耗时 (timeout-kill 路径无此行, 由 control 侧 timeout 日志补)
        long dur = System.currentTimeMillis() - curStartMs;
        Log.i(TAG, "[SpiderPerf] role=" + role() + " pid=" + android.os.Process.myPid()
                + " method=" + curMethod + " class=" + com.quantumtv.bridge.ipc.JsonLite.string(body, "class")
                + " duration=" + dur + "ms status=" + (r.code == 200 ? "ok" : "code" + r.code));
        send(Proto.T_RESP, reqId,
                respJson(reqId, r.code, r.err, r.data).getBytes(StandardCharsets.UTF_8));
        state = WorkerState.IDLE; curReqId = -1; curMethod = ""; curStartMs = 0;
    }

    /** cookie/ext 下发 (决策#2): {"ext":"...","quark":"...","uc":"...","baidu":"..."} */
    protected void onCookie(String json) {
        String ext = com.quantumtv.bridge.ipc.JsonLite.string(json, "ext");
        if (ext != null) SpiderExec.get().reinit(ext);
        for (String drive : new String[]{"quark", "uc", "baidu"}) {
            String cookie = com.quantumtv.bridge.ipc.JsonLite.string(json, drive);
            if (cookie != null && !cookie.isEmpty()) SpiderExec.get().applyCookie(drive, cookie);
        }
    }

    /** data 为原始字符串; 统一 JSON 转义后以字符串字面量嵌入 (与 Control stringField 的 unescape 配对) */
    static String respJson(int reqId, int code, String err, String data) {
        StringBuilder sb = new StringBuilder("{\"reqId\":").append(reqId).append(",\"code\":").append(code);
        if (err != null) sb.append(",\"err\":\"").append(com.quantumtv.bridge.ipc.JsonLite.escape(err)).append("\"");
        if (data != null) sb.append(",\"data\":\"").append(com.quantumtv.bridge.ipc.JsonLite.escape(data)).append("\"");
        return sb.append("}").toString();
    }

    private void startHeartbeat() {
        Thread hb = new Thread(() -> {
            while (running && out != null) {
                String body = "{\"state\":\"" + state.name() + "\",\"curReqId\":" + curReqId
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
        try {
            synchronized (o) {
                o.write(Proto.encode(reqId, type, payload));
                o.flush();
            }
        } catch (Exception e) {
            Log.w(TAG, role() + " send: " + e);
        }
    }

    @Override public void onDestroy() {
        running = false;
        if (execThread != null) execThread.interrupt(); // 退出信号; 卡死时由控制面 killProcess (§21)
        super.onDestroy();
    }
}
