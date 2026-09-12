package com.quantumtv.bridge.control;

import android.content.Context;
import android.content.Intent;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.util.Log;

import com.quantumtv.bridge.ipc.Proto;
import com.quantumtv.bridge.ipc.TimeoutPolicy;
import com.quantumtv.bridge.ipc.WorkerState;

import java.io.DataInputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayDeque;
import java.util.Deque;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.function.Consumer;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Control 侧 Worker 全生命周期 (方案 §7/§19-§22/§31/§40/§62/§76):
 * 拉起/登记/派发/硬超时 Watchdog/kill+重启/崩溃冷却。Watchdog 永远在 Control (§20)。
 */
public final class WorkerManager {
    private static final String TAG = "BridgeCtl";
    public static final String ROLE_GENERAL = "general";
    public static final String ROLE_PLAYBACK = "playback";

    public static final class Handle {
        public volatile WorkerState state = WorkerState.STARTING;
        public volatile int pid = -1;
        public volatile java.io.OutputStream out;
        public volatile long lastHbMs;
    }

    public static final class Resp {
        public final int code;
        public final String err;
        public final String data;
        public Resp(int c, String e, String d) { code = c; err = e; data = d; }
    }

    private static final class Pending {
        final String role, method;
        final long startMs;
        final Consumer<Resp> done;
        Pending(String r, String m, long t, Consumer<Resp> d) { role = r; method = m; startMs = t; done = d; }
    }

    private final Map<String, Handle> workers = new ConcurrentHashMap<>();
    private final Map<Integer, Pending> pending = new ConcurrentHashMap<>();
    private final Map<String, Deque<Long>> deaths = new ConcurrentHashMap<>(); // §40
    private final AtomicInteger nextReqId = new AtomicInteger(1);
    private final Context ctx;
    private volatile boolean running = true;

    public WorkerManager(Context ctx) {
        this.ctx = ctx;
        workers.put(ROLE_GENERAL, new Handle());
        workers.put(ROLE_PLAYBACK, new Handle());
    }

    public void start() {
        for (String role : workers.keySet()) {
            new Thread(() -> listen(role), "CtlListen-" + role).start();
            new Thread(this::watchdogLoop, "WorkerWatchdog").start();
            spawn(role);
        }
    }

    private void spawn(String role) {
        Class<?> cls = role.equals(ROLE_GENERAL)
                ? com.quantumtv.bridge.worker.GeneralWorkerService.class
                : com.quantumtv.bridge.worker.PlaybackWorkerService.class;
        ctx.startService(new Intent(ctx, cls));
        Log.i(TAG, "spawn worker " + role);
    }

    /** 派发一个调用; done 可能在 listen 线程或 watchdog 线程回调。 */
    public void dispatch(String role, String method, byte[] reqJson, Consumer<Resp> done) {
        Handle h = workers.get(role);
        if (h == null) { done.accept(new Resp(503, "worker_unavailable", null)); return; }
        synchronized (h) {
            if (h.state == WorkerState.DISABLED) { done.accept(new Resp(503, "worker_disabled", null)); return; }
            if (!h.state.canAcceptRequest()) { done.accept(new Resp(503, "worker_restarting", null)); return; }
            int rid = nextReqId.getAndIncrement();
            pending.put(rid, new Pending(role, method, System.currentTimeMillis(), done));
            h.state = WorkerState.BUSY;
            h.lastHbMs = System.currentTimeMillis();
            try {
                synchronized (h.out) {
                    h.out.write(Proto.encode(rid, Proto.T_REQ, reqJson));
                    h.out.flush();
                }
            } catch (Exception e) {
                failPending(rid, 502, "worker_send_failed");
            }
        }
    }

    private void failPending(int rid, int code, String err) {
        Pending p = pending.remove(rid);
        if (p != null) {
            Handle h = workers.get(p.role);
            if (h != null && (h.state == WorkerState.BUSY || h.state == WorkerState.SUSPECT)) {
                h.state = WorkerState.IDLE;
            }
            p.done.accept(new Resp(code, err, null));
        }
    }

    private void listen(String role) {
        try (LocalServerSocket srv = new LocalServerSocket("qtv.bridge.ctl." + role)) {
            while (running) {
                LocalSocket s = srv.accept();
                connectionLoop(role, s);
            }
        } catch (Exception e) {
            if (running) Log.e(TAG, "listen " + role + ": " + e);
        }
    }

    private void connectionLoop(String role, LocalSocket s) {
        Handle h = workers.get(role);
        try {
            h.out = s.getOutputStream();
            DataInputStream in = new DataInputStream(s.getInputStream());
            byte[] hdr = new byte[12];
            while (running) {
                in.readFully(hdr);
                int reqId = ((hdr[0] & 255) << 24) | ((hdr[1] & 255) << 16) | ((hdr[2] & 255) << 8) | (hdr[3] & 255);
                int type = ((hdr[4] & 255) << 24) | ((hdr[5] & 255) << 16) | ((hdr[6] & 255) << 8) | (hdr[7] & 255);
                int len = ((hdr[8] & 255) << 24) | ((hdr[9] & 255) << 16) | ((hdr[10] & 255) << 8) | (hdr[11] & 255);
                if (len < 0 || len > Proto.MAX_PAYLOAD) break;
                byte[] payload = new byte[len];
                in.readFully(payload);
                String body = new String(payload, StandardCharsets.UTF_8);
                switch (type) {
                    case Proto.T_HELLO:
                        h.pid = fieldInt(body, "pid");
                        h.lastHbMs = System.currentTimeMillis();
                        Log.i(TAG, "worker " + role + " hello pid=" + h.pid);
                        break;
                    case Proto.T_READY:
                        h.state = WorkerState.IDLE;
                        Log.i(TAG, "[Bridge] worker=" + role + " pid=" + h.pid + " status=ready");
                        break;
                    case Proto.T_RESP:
                        Pending p = pending.remove(fieldInt(body, "reqId"));
                        if (h.state == WorkerState.BUSY || h.state == WorkerState.SUSPECT) {
                            h.state = WorkerState.IDLE;
                        }
                        if (p != null) {
                            p.done.accept(new Resp(fieldInt(body, "code"),
                                    stringField(body, "err"), stringField(body, "data")));
                        }
                        break;
                    case Proto.T_HB:
                        h.lastHbMs = System.currentTimeMillis();
                        String st = stringField(body, "state");
                        if (h.state == WorkerState.SUSPECT && ("IDLE".equals(st) || "BUSY".equals(st))) {
                            h.state = WorkerState.valueOf(st); // §19 心跳恢复即自愈
                        }
                        break;
                    default:
                        break;
                }
            }
        } catch (Exception e) {
            Log.w(TAG, "worker " + role + " link closed: " + e);
        }
        synchronized (h) {
            // kill/restart 过程中链路必然 EOF: 不覆盖显式状态, 其余按死亡处理 (§62: worker 死 ≠ control 死)
            if (h.state != WorkerState.KILLING && h.state != WorkerState.RESTARTING
                    && h.state != WorkerState.DISABLED) {
                h.state = WorkerState.DEAD;
                h.out = null;
                Log.w(TAG, "[Bridge] worker=" + role + " pid=" + h.pid + " status=dead");
            }
        }
    }

    /** §19/§20/§21/§31: 心跳 3s→SUSPECT(停发新请求); 超硬超时→kill+restart, 在途请求回 worker_killed (§35)。 */
    private void watchdogLoop() {
        while (running) {
            long now = System.currentTimeMillis();
            for (Map.Entry<Integer, Pending> e : pending.entrySet()) {
                Pending p = e.getValue();
                Handle h = workers.get(p.role);
                if (h == null) continue;
                long hard = TimeoutPolicy.hardMs(p.method);
                boolean overdue = now - p.startMs > hard;
                boolean hbStale = now - h.lastHbMs > 5000;
                if (overdue || hbStale) {
                    long age = now - p.startMs;
                    Log.w(TAG, "[SpiderWorker] worker=" + p.role + " pid=" + h.pid + " method=" + p.method
                            + " duration=" + age + "ms status=timeout action=kill");
                    killAndRestart(p.role, h);
                    failPending(e.getKey(), 503, "worker_killed");
                    break; // 每轮处理一个
                } else if (now - h.lastHbMs > 3000 && h.state == WorkerState.BUSY) {
                    h.state = WorkerState.SUSPECT;
                }
            }
            for (Map.Entry<String, Handle> en : workers.entrySet()) {
                Handle h = en.getValue();
                if (h.state == WorkerState.DEAD) restart(en.getKey(), h, "link_dead");
            }
            try { Thread.sleep(500); } catch (InterruptedException ignored) { return; }
        }
    }

    private void killAndRestart(String role, Handle h) {
        synchronized (h) {
            h.state = WorkerState.KILLING;
            try { android.os.Process.killProcess(h.pid); } catch (Exception ignored) { }
            Log.i(TAG, "[Bridge] worker=" + role + " pid=" + h.pid + " status=killed");
            restart(role, h, "killed");
        }
    }

    /** §40 冷却: 60s 窗口第 3 次死亡 → DISABLED, 防夸克挂死引发重启风暴。 */
    private void restart(String role, Handle h, String reason) {
        Deque<Long> d = deaths.computeIfAbsent(role, k -> new ArrayDeque<>());
        long now = System.currentTimeMillis();
        synchronized (d) {
            while (!d.isEmpty() && now - d.peekFirst() > 60_000) d.pollFirst();
            d.addLast(now);
            if (d.size() >= 3) {
                h.state = WorkerState.DISABLED;
                Log.e(TAG, "[Bridge] worker=" + role + " status=disabled reason=crash_loop(§40)");
                return;
            }
        }
        h.state = WorkerState.RESTARTING;
        Log.i(TAG, "[Bridge] worker=" + role + " status=restarting reason=" + reason);
        spawn(role);
    }

    static int fieldInt(String json, String key) {
        try {
            int i = json.indexOf("\"" + key + "\":");
            if (i < 0) return -1;
            int s = i + key.length() + 3;
            while (s < json.length() && (json.charAt(s) == '-' || json.charAt(s) == '"')) s++;
            int e = s;
            while (e < json.length() && Character.isDigit(json.charAt(e))) e++;
            return Integer.parseInt(json.substring(s, e));
        } catch (Exception ex) {
            return -1;
        }
    }

    static String stringField(String json, String key) {
        String pat = "\"" + key + "\":\"";
        int i = json.indexOf(pat);
        if (i < 0) return null;
        int s = i + pat.length();
        int e = json.indexOf('"', s);
        return e < 0 ? null : json.substring(s, e);
    }

    public WorkerState state(String role) {
        Handle h = workers.get(role);
        return h == null ? WorkerState.DEAD : h.state;
    }

    public Map<String, Integer> workerPids() {
        Map<String, Integer> m = new ConcurrentHashMap<>();
        for (Map.Entry<String, Handle> e : workers.entrySet()) m.put(e.getKey(), e.getValue().pid);
        return m;
    }

    public void shutdown() { running = false; }
}
