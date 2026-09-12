package com.quantumtv.bridge.control;

import android.content.Context;
import android.content.Intent;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.util.Log;

import com.quantumtv.bridge.ipc.Proto;
import com.quantumtv.bridge.ipc.WorkerState;

import java.io.DataInputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;

/**
 * Control 侧 Worker 生命周期 (方案 §7/§20/§22/§76): 拉起/登记/监控/重启。
 * 本任务范围: LocalServerSocket + HELLO/READY 登记; 派发/超时/kill 在 T4 接入。
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

    public static final class Resp {
        public final int code;
        public final String err;
        public final String data;
        public Resp(int c, String e, String d) { code = c; err = e; data = d; }
    }

    private final Map<String, Handle> workers = new ConcurrentHashMap<>();
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

    private void listen(String role) {
        try (LocalServerSocket srv = new LocalServerSocket("qtv.bridge.ctl." + role)) {
            while (running) {
                LocalSocket s = srv.accept();
                connectionLoop(role, s); // 每条链路独立 try/catch, 一路 EOF 不杀监听
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
                int type = ((hdr[4] & 255) << 24) | ((hdr[5] & 255) << 16) | ((hdr[6] & 255) << 8) | (hdr[7] & 255);
                int len = ((hdr[8] & 255) << 24) | ((hdr[9] & 255) << 16) | ((hdr[10] & 255) << 8) | (hdr[11] & 255);
                if (len < 0 || len > Proto.MAX_PAYLOAD) break;
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
                // HB/RESP 在 T4 接入
            }
        } catch (Exception e) {
            Log.w(TAG, "worker " + role + " link closed: " + e);
        }
        h.state = WorkerState.DEAD;
        h.out = null;
        Log.w(TAG, "[Bridge] worker=" + role + " pid=" + h.pid + " status=dead");
    }

    static int fieldInt(String json, String key) {
        try {
            int i = json.indexOf("\"" + key + "\":");
            if (i < 0) return -1;
            int s = i + key.length() + 3;
            int e = s;
            while (e < json.length() && Character.isDigit(json.charAt(e))) e++;
            return Integer.parseInt(json.substring(s, e));
        } catch (Exception ex) {
            return -1;
        }
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
