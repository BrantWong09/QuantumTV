package com.quantumtv.bridge;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;
import android.webkit.CookieManager;

import com.quantumtv.bridge.control.WorkerManager;
import com.quantumtv.bridge.ipc.JsonLite;
import com.quantumtv.bridge.worker.SpiderExec;

import java.io.OutputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.Executors;
import java.util.concurrent.ExecutorService;

/**
 * Bridge Control 进程 (方案 §7): 只负责桌面 TCP 隧道/兼容 8080、路由、Worker 生命周期、健康。
 * 不再执行任何 spider 调用 (§23/§27/§49): /init /health 与 spider 卡死彻底解耦;
 * spider 调用全部派发到 :spider_general / :spider_playback 独立进程 (WorkerManager)。
 */
public class BridgeService extends Service {
    private static final String TAG = "Bridge";
    private static final int PORT = 8080;
    private ServerSocket server;
    ExecutorService pool;
    /** 控制面 Worker 生命周期管理: 派发/Watchdog/重启/冷却 */
    public WorkerManager workers;
    /** 类级 playerContent 熔断 (§41/§42): 连续 3 次超时 → 30s 内该 class 秒拒 */
    private final com.quantumtv.bridge.control.SourceBreaker breaker =
        new com.quantumtv.bridge.control.SourceBreaker(30_000, System::currentTimeMillis);
    private volatile boolean initialized = false;
    /** 站点级 ext 配置, /init 时由桌面端传入, 随 cookie 一并下发 worker (§决策#2) */
    private volatile String extConfig = "";
    /** 网盘 cookie 留存, worker (重)启动时显式补发 */
    private final Map<String, String> cookieStore = new ConcurrentHashMap<>();

    /**
     * 网盘账号在 WebView 登录后调用: 广播 cookie/ext 到全部 worker, 各自清缓存重建 (§28)。
     */
    public static void invalidateSpiders() {
        BridgeService instance = runningInstance;
        if (instance == null) return;
        instance.workers.broadcastCookie(instance.extConfig, instance.cookieStore);
        Log.i(TAG, "cookie/ext 已广播至 workers, spider 实例将重建");
    }

    private static BridgeService runningInstance;

    @Override
    public void onCreate() {
        super.onCreate();
        runningInstance = this;
    }

    @Override
    public IBinder onBind(Intent intent) { return null; }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        startForeground();
        if (server != null) return START_STICKY;
        try {
            server = new ServerSocket(PORT);
            Log.i(TAG, "HTTP server listening on " + PORT);
            pool = Executors.newFixedThreadPool(4);
            new Thread(this::acceptLoop, "BridgeAccept").start();
            workers = new WorkerManager(this);
            // worker (重)启动就绪后补发 cookie/ext, 不等桌面 /init (§22/决策#2)
            workers.readyHook = role -> workers.broadcastCookie(extConfig, cookieStore);
            workers.start();
            TunnelClient.start(this);
        } catch (Exception e) {
            Log.e(TAG, "server start failed: " + e);
        }
        return START_STICKY;
    }

    private void startForeground() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            NotificationChannel ch = new NotificationChannel("bridge", "Bridge", NotificationManager.IMPORTANCE_LOW);
            getSystemService(NotificationManager.class).createNotificationChannel(ch);
            Notification n = new Notification.Builder(this, "bridge")
                .setContentTitle("QuantumTV Bridge")
                .setContentText("running")
                .setSmallIcon(android.R.drawable.stat_notify_sync)
                .build();
            startForeground(1, n);
        }
    }

    private void acceptLoop() {
        while (!server.isClosed()) {
            try {
                Socket s = server.accept();
                pool.submit(() -> handle(s));
            } catch (Exception e) {
                Log.e(TAG, "accept: " + e);
            }
        }
    }

    /** 从原始流读一行(以 \n 结尾, 去掉尾部 \r); 全程按字节, 不经缓冲 Reader, 避免预读吃掉 body 字节 */
    private static String readLineRaw(java.io.InputStream in) throws Exception {
        java.io.ByteArrayOutputStream buf = new java.io.ByteArrayOutputStream();
        int b;
        boolean any = false;
        while ((b = in.read()) != -1) {
            any = true;
            if (b == '\n') break;
            buf.write(b);
        }
        if (!any) return null;
        String s = buf.toString("UTF-8");
        return s.endsWith("\r") ? s.substring(0, s.length() - 1) : s;
    }

    /** 隧道帧 payload (原始 HTTP 请求字节) → [method, path, body]; 解析失败返回 null */
    static String[] parseRequest(byte[] raw) {
        try {
            java.io.ByteArrayInputStream in = new java.io.ByteArrayInputStream(raw);
            String line = readLineRaw(in);
            if (line == null) return null;
            String[] parts = line.split(" ");
            if (parts.length < 2) return null;
            int contentLength = 0;
            String h;
            while ((h = readLineRaw(in)) != null && !h.isEmpty()) {
                if (h.toLowerCase().startsWith("content-length:")) {
                    contentLength = Integer.parseInt(h.split(":", 2)[1].trim());
                }
            }
            byte[] body = new byte[contentLength];
            int read = 0;
            while (read < contentLength) {
                int n = in.read(body, read, contentLength - read);
                if (n < 0) break;
                read += n;
            }
            return new String[]{parts[0], parts[1], new String(body, 0, read, java.nio.charset.StandardCharsets.UTF_8)};
        } catch (Exception e) {
            return null;
        }
    }

    /** 路由分发 (控制面): spider 类操作派发至独立进程, 其余 control 自答 (§7/§23/§24)。 */
    String routeRequest(String method, String path, String body) {
        if ("/health".equals(path)) {
            // §24/§48: control 自答, 绝不触碰 spider 执行体; worker 挂死时 bridge 仍 healthy
            return json(200, "ok", "{\"bridge\":\"healthy\",\"init\":" + initialized
                    + ",\"workers\":{\"general\":\"" + workerLabel(WorkerManager.ROLE_GENERAL)
                    + "\",\"playback\":\"" + workerLabel(WorkerManager.ROLE_PLAYBACK) + "\"}"
                    + ",\"sources_open\":" + (workers == null ? "{}" : breaker.snapshot()) + "}");
        }
        if ("/__test_hang".equals(path) || "/__test_freeze".equals(path)) {
            // 隔离验收钩子 (§58/§60/§61): 仅 debuggable APK; 按 playerContent 记入类级熔断
            if ((getApplicationInfo().flags & android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) == 0) {
                return json(404, "not_found", null);
            }
            final boolean freeze = path.equals("/__test_freeze");
            final String cls = parseField(body, "class") == null
                    ? (freeze ? "WextestFreeze" : "WextestHang") : parseField(body, "class");
            final String role = freeze && "general".equals(parseField(body, "role"))
                    ? WorkerManager.ROLE_GENERAL : WorkerManager.ROLE_PLAYBACK;
            final String m = freeze ? "__test_freeze" : "__test_hang";
            java.util.concurrent.CompletableFuture<String> f = new java.util.concurrent.CompletableFuture<>();
            byte[] req;
            try {
                String msPart = freeze ? ",\"ms\":" + Math.max(100, Math.min(
                        parseNum(body, "ms", 8000L), 60_000)) : "";
                req = ("{\"method\":\"" + m + "\",\"class\":\"" + JsonLite.escape(cls) + "\"" + msPart + "}")
                        .getBytes("UTF-8");
            } catch (Exception e) { return json(500, "enc", null); }
            workers.dispatch(role, m, req, r -> {
                if (r.code == 503 && "worker_killed".equals(r.err)) breaker.recordPlayerContentTimeout(cls);
                else if (r.code == 200) breaker.recordPlayerContentSuccess(cls);
                f.complete(json(r.code, r.err, r.data == null ? null : JsonLite.quote(r.data)));
            });
            try { return f.get(120, java.util.concurrent.TimeUnit.SECONDS); }
            catch (Exception e) { return json(500, "hang_dispatch_failed", null); }
        }
        if ("/__test_config".equals(path) && "POST".equalsIgnoreCase(method)) {
            if ((getApplicationInfo().flags & android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) == 0) {
                return json(404, "not_found", null);
            }
            try {
                long v = parseNum(body, "disable_recovery_ms", -1);
                if (v > 0) WorkerManager.DISABLE_RECOVERY_MS = Math.max(500, v);
            } catch (Exception e) { return json(400, "bad_config", null); }
            return json(200, "ok", "{\"disable_recovery_ms\":" + WorkerManager.DISABLE_RECOVERY_MS + "}");
        }
        if (isSpiderOp(path) && "POST".equalsIgnoreCase(method)) {
            return dispatchSpider(path, body);
        }
        if ("/init".equals(path) && "POST".equalsIgnoreCase(method)) return doInit(body);
        if ("/setCookie".equals(path) && "POST".equalsIgnoreCase(method)) return doSetCookie(body);
        return json(404, "not_found", null);
    }

    private static boolean isSpiderOp(String path) {
        return "/search".equals(path) || "/playerContent".equals(path) || "/detail".equals(path)
                || "/home".equals(path) || "/category".equals(path);
    }

    /** spider 调用统一派发 (§15/§16/§44): playerContent→playback worker, 其余→general worker。 */
    private String dispatchSpider(String path, String body) {
        final boolean pc = "/playerContent".equals(path);
        String role = pc ? WorkerManager.ROLE_PLAYBACK : WorkerManager.ROLE_GENERAL;
        final String cls = parseField(body, "class");
        if (cls == null) return json(400, "missing class", null);
        if (pc && !breaker.allowPlayerContent(cls)) {
            Log.i(TAG, "source_circuit_open class=" + cls);
            return json(503, "source_circuit_open", null);
        }
        final String m = path.substring(1);
        final long t0 = System.currentTimeMillis();
        java.util.concurrent.CompletableFuture<String> f = new java.util.concurrent.CompletableFuture<>();
        byte[] req;
        try {
            req = ipcReq(m, cls, body).getBytes("UTF-8");
        } catch (Exception e) { return json(500, "ipc_encode_failed", null); }
        workers.dispatch(role, m, req, r -> {
            if (pc) {
                if (r.code == 503 && "worker_killed".equals(r.err)) breaker.recordPlayerContentTimeout(cls);
                else if (r.code == 200) breaker.recordPlayerContentSuccess(cls);
            }
            // §70: 控制面对账 (排队+IPC+执行的总时长; 与 worker 侧差值即调度/IPC 开销)
            Log.i(TAG, "[SpiderPerf] side=control role=" + role + " method=" + m + " class=" + cls
                    + " duration=" + (System.currentTimeMillis() - t0) + "ms status="
                    + (r.code == 200 ? "ok" : String.valueOf(r.err)));
            f.complete(json(r.code, r.err, r.data == null ? null : JsonLite.quote(r.data)));
        });
        try { return f.get(150, java.util.concurrent.TimeUnit.SECONDS); }
        catch (Exception e) { return json(500, "dispatch_failed", null); }
    }

    /** 桌面 body → worker REQ 载荷 (§8): 保留原 method 语义的字段映射。 */
    private static String ipcReq(String m, String cls, String body) {
        StringBuilder sb = new StringBuilder("{\"method\":\"").append(m)
                .append("\",\"class\":\"").append(JsonLite.escape(cls)).append("\"");
        String v;
        if ((v = parseField(body, "keyword")) != null) sb.append(",\"keyword\":\"").append(JsonLite.escape(v)).append("\"");
        if ((v = parseField(body, "ids")) != null) sb.append(",\"ids\":\"").append(JsonLite.escape(v)).append("\"");
        if ((v = parseField(body, "id")) != null) sb.append(",\"id\":\"").append(JsonLite.escape(v)).append("\"");
        if ((v = parseField(body, "flag")) != null) sb.append(",\"flag\":\"").append(JsonLite.escape(v)).append("\"");
        if ((v = parseField(body, "tid")) != null) sb.append(",\"tid\":\"").append(JsonLite.escape(v)).append("\"");
        if ((v = parseField(body, "pg")) != null) sb.append(",\"pg\":\"").append(JsonLite.escape(v)).append("\"");
        return sb.append("}").toString();
    }

    /** §48: worker 状态标签 (health 与控制面解耦)。 */
    private String workerLabel(String role) {
        if (workers == null) return "starting";
        com.quantumtv.bridge.ipc.WorkerState s = workers.state(role);
        switch (s) {
            case IDLE: case BUSY: return "ready";
            case STARTING: return "starting";
            case SUSPECT: case KILLING: case RESTARTING: return "restarting";
            case DEAD: return "dead";
            case DISABLED: return "disabled";
            default: return "unknown";
        }
    }

    private void handle(Socket s) {
        try {
            // 注意: Content-Length 是字节数, 必须按字节读完整 body 再按 UTF-8 解码;
            // 用 Reader 按 char 读会在中文(多字节)请求上凑不满字符数而永久阻塞
            java.io.InputStream in = s.getInputStream();
            String line = readLineRaw(in);
            if (line == null) { s.close(); return; }
            String[] parts = line.split(" ");
            String method = parts[0];
            String path = parts[1];
            int contentLength = 0;
            while ((line = readLineRaw(in)) != null && !line.isEmpty()) {
                if (line.toLowerCase().startsWith("content-length:")) {
                    contentLength = Integer.parseInt(line.split(":", 2)[1].trim());
                }
            }
            byte[] body = new byte[contentLength];
            int read = 0;
            while (read < contentLength) {
                int n = in.read(body, read, contentLength - read);
                if (n < 0) break;
                read += n;
            }
            String bodyStr = new String(body, 0, read, java.nio.charset.StandardCharsets.UTF_8);
            Log.i(TAG, method + " " + path + " body=" + bodyStr);
            String resp = routeRequest(method, path, bodyStr);
            writeHttp(s, resp);
            s.close();
        } catch (Exception e) {
            Log.e(TAG, "handle: " + e);
            try { s.close(); } catch (Exception ex) {}
        }
    }

    private void writeHttp(Socket s, String body) throws Exception {
        byte[] bodyBytes = body.getBytes(java.nio.charset.StandardCharsets.UTF_8);
        OutputStream out = s.getOutputStream();
        String headers = "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: " + bodyBytes.length + "\r\nConnection: close\r\n\r\n";
        out.write(headers.getBytes(java.nio.charset.StandardCharsets.US_ASCII));
        out.write(bodyBytes);
        out.flush();
    }

    private String json(int code, String err, String data) {
        StringBuilder sb = new StringBuilder("{\"code\":");
        sb.append(code);
        if (err != null) sb.append(",\"err\":\"").append(esc(err)).append("\"");
        if (data != null) sb.append(",\"data\":").append(data);
        sb.append("}");
        return sb.toString();
    }

    private String esc(String s) { return s.replace("\\", "\\\\").replace("\"", "\\\""); }

    /**
     * §23/§45/§46: /init 由 control 秒回, ext 下发 worker; native 库下载/Init.init 在 worker
     * 首次调用前后台自举 (§78)。worker 启动失败 bridge 仍 healthy, 只反映在 health 结构 (§47)。
     */
    private String doInit(String body) {
        String ext = (body != null && !body.isEmpty()) ? parseField(body, "ext") : null;
        if (ext != null) extConfig = ext;
        initialized = true; // 控制面就绪 ≠ spider 就绪 (§46)
        workers.broadcastCookie(extConfig, cookieStore);
        return json(200, null, "{\"ok\":true,\"bridge\":\"ready\",\"worker\":\"starting\"}");
    }

    /** 桌面端扫码登录: cookie 留存 → 写本进程 CookieManager + 文件 → 显式下发全部 worker (§决策#2) */
    private String doSetCookie(String body) {
        try {
            String drive = parseField(body, "drive");
            String cookie = parseField(body, "cookie");
            if (drive == null || cookie == null || cookie.isEmpty()) {
                return json(400, "missing drive/cookie", null);
            }
            String[] hosts = SpiderExec.cookieHosts(drive);
            if (hosts == null) {
                return json(400, "unsupported drive: " + drive, null);
            }
            CookieManager cm = CookieManager.getInstance();
            cm.setAcceptCookie(true);
            for (String h : hosts) {
                cm.setCookie("https://" + h + "/", cookie);
            }
            cm.flush();
            SpiderExec.writeCookieFile(this, drive, cookie);
            cookieStore.put(drive, cookie);
            workers.broadcastCookie(extConfig, cookieStore);
            Log.i(TAG, "setCookie: drive=" + drive + " len=" + cookie.length() + " 已下发 worker");
            return json(200, "ok", null);
        } catch (Exception e) {
            Log.e(TAG, "setCookie", e);
            return json(500, "set_cookie_failed", null);
        }
    }

    /** 兼容 CloudLoginActivity 静态入口 (主进程文件兜底通道) */
    public static void writeCookieFile(android.content.Context ctx, String drive, String cookies) {
        SpiderExec.writeCookieFile(ctx, drive, cookies);
    }

    private static String parseField(String body, String key) {
        // very minimal JSON parse for string fields (与 worker JsonLite 同语义)
        String pat = "\"" + key + "\":\"";
        int i = body.indexOf(pat);
        if (i < 0) return null;
        int s = i + pat.length();
        int e = body.indexOf("\"", s);
        return e < 0 ? null : body.substring(s, e);
    }

    private static long parseNum(String body, String key, long def) {
        String pat = "\"" + key + "\":";
        int i = body.indexOf(pat);
        if (i < 0) return def;
        int s = i + pat.length();
        while (s < body.length() && (body.charAt(s) == ' ' || body.charAt(s) == '"')) s++;
        int e = s;
        while (e < body.length() && Character.isDigit(body.charAt(e))) e++;
        if (e == s) return def;
        try { return Long.parseLong(body.substring(s, e)); } catch (Exception ex) { return def; }
    }

    @Override
    public void onDestroy() {
        if (runningInstance == this) runningInstance = null;
        if (workers != null) workers.shutdown();
        try { if (server != null) server.close(); } catch (Exception e) {}
        super.onDestroy();
    }
}
