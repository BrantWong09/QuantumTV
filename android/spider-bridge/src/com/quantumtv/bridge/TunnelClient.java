package com.quantumtv.bridge;

import android.os.Build;
import android.util.Log;

import java.io.BufferedInputStream;
import java.io.DataInputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.net.Socket;

/**
 * 桥接隧道客户端: 主动拨宿主网关 18099, 与桌面端建立持久帧隧道。
 * 帧格式 [u32 BE id][u32 BE len][payload]; 首帧 id=0 注册 JSON;
 * 桌面→APK 帧 payload = 完整 HTTP 请求字节; APK 回 [id][len][响应体]。
 * 断线自动重拨: 全部网关失败时 5s 起指数退避, 上限 30s; 成功后复位 5s。
 * 网关候选: 10.0.2.2 (QEMU 系) → ip route default via → 192.168.56.1 (VirtualBox 系)。
 */
public class TunnelClient implements Runnable {
    static final int PORT = 18099;
    private static final String TAG = "BridgeTunnel";
    private final BridgeService svc;
    private volatile boolean running = true;

    private TunnelClient(BridgeService svc) {
        this.svc = svc;
    }

    static void start(BridgeService svc) {
        Thread t = new Thread(new TunnelClient(svc), "BridgeTunnel");
        t.setDaemon(true);
        t.start();
    }

    @Override
    public void run() {
        long backoff = 5000;
        while (running) {
            boolean connected = false;
            for (String host : gateways()) {
                if (!running) return;
                try (Socket sock = new Socket()) {
                    sock.connect(new InetSocketAddress(host, PORT), 3000);
                    sock.setTcpNoDelay(true);
                    session(sock);
                    connected = true;
                    break;
                } catch (Exception e) {
                    Log.d(TAG, "dial " + host + " failed: " + e);
                }
            }
            sleep(connected ? 5000 : backoff);
            backoff = connected ? 5000 : Math.min(backoff * 2, 30000);
        }
    }

    private void session(Socket sock) throws Exception {
        OutputStream out = sock.getOutputStream();
        DataInputStream in = new DataInputStream(new BufferedInputStream(sock.getInputStream(), 16 * 1024));
        String reg = "{\"device\":\"" + Build.MODEL + "\",\"apk\":\"1.1\"}";
        writeFrame(out, 0, reg.getBytes(java.nio.charset.StandardCharsets.UTF_8));
        Log.i(TAG, "tunnel registered to host");
        while (running) {
            int id = in.readInt();
            int len = in.readInt();
            if (len < 0 || len > 8 * 1024 * 1024) break;
            byte[] payload = new byte[len];
            in.readFully(payload);
            final int fid = id;
            final byte[] fpayload = payload;
            final String path = framePath(payload);
            Runnable job = () -> {
                try {
                    String[] mpb = BridgeService.parseRequest(fpayload);
                    String resp = mpb == null
                        ? "{\"code\":400,\"err\":\"bad_request\"}"
                        : svc.routeRequest(mpb[0], mpb[1], mpb[2]);
                    synchronized (out) {
                        writeFrame(out, fid, resp.getBytes(java.nio.charset.StandardCharsets.UTF_8));
                    }
                } catch (Exception e) {
                    Log.e(TAG, "frame handle: " + e);
                }
            };
            // 统一交 control 路由: spider 操作会被派发至对应 worker 进程, control 线程池不再区分优先级 (§15)
            svc.pool.submit(job);
        }
        Log.w(TAG, "tunnel closed by host");
    }

    private static String framePath(byte[] raw) {
        try {
            java.io.ByteArrayInputStream in = new java.io.ByteArrayInputStream(raw);
            byte[] buf = new byte[512];
            int n = 0, b;
            while ((b = in.read()) != -1 && b != '\n' && n < buf.length) buf[n++] = (byte) b;
            String line = new String(buf, 0, n, java.nio.charset.StandardCharsets.UTF_8).trim();
            String[] parts = line.split(" ");
            return parts.length >= 2 ? parts[1] : "";
        } catch (Exception e) {
            return "";
        }
    }

    private static void writeFrame(OutputStream out, int id, byte[] payload) throws Exception {
        out.write(intBytes(id));
        out.write(intBytes(payload.length));
        out.write(payload);
        out.flush();
    }

    private static byte[] intBytes(int v) {
        return new byte[]{(byte) (v >>> 24), (byte) (v >>> 16), (byte) (v >>> 8), (byte) v};
    }

    private static String[] gateways() {
        java.util.LinkedHashSet<String> list = new java.util.LinkedHashSet<>();
        list.add("10.0.2.2");
        try {
            java.util.Scanner sc = new java.util.Scanner(
                Runtime.getRuntime().exec(new String[]{"sh", "-c", "ip route show"}).getInputStream());
            while (sc.hasNextLine()) {
                String line = sc.nextLine();
                if (line.startsWith("default")) {
                    String[] p = line.split("\\s+");
                    for (int i = 0; i < p.length - 1; i++) {
                        if ("via".equals(p[i])) list.add(p[i + 1]);
                    }
                }
            }
            sc.close();
        } catch (Exception ignored) {
        }
        list.add("192.168.56.1");
        return list.toArray(new String[0]);
    }

    private static void sleep(long ms) {
        try {
            Thread.sleep(ms);
        } catch (InterruptedException ignored) {
        }
    }
}
