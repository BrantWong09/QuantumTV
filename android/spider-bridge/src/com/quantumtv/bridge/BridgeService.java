package com.quantumtv.bridge;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.lang.reflect.Method;
import java.net.ServerSocket;
import java.net.Socket;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.Executors;
import java.util.concurrent.ExecutorService;

public class BridgeService extends Service {
    private static final String TAG = "Bridge";
    private static final int PORT = 8080;
    /** wex 系 spider 的配置入口 (AES 加密返回配置服务器地址) */
    private static final String WEX_CONFIG_URL = "https://9280.kstore.vip/api.txt";
    private ServerSocket server;
    private ExecutorService pool;
    /** /detail 专用单线程池: 与搜索队列隔离, 保证点击播放低延迟 */
    private ExecutorService detailExecutor;
    private boolean initialized = false;
    /** 站点级 ext 配置, /init 时由桌面端传入; TVBoxOSC 在 getSpider 后调用 spider.init(context, ext) */
    private volatile String extConfig = "";
    /** 已初始化的 spider 实例缓存: TVBoxOSC 同样按 jar+site 缓存, init 只跑一次 */
    private final Map<String, Object> spiderCache = new HashMap<>();
    /** wex 系 spider 的 native 链 (DexNative/libLoadNiMa) 非线程安全, 并发调用会 SIGABRT, 必须串行 */
    private final Object spiderLock = new Object();
    /** detail 优先标记: >0 时搜索线程在调用间隙主动让出 spiderLock */
    private final java.util.concurrent.atomic.AtomicInteger detailPending =
        new java.util.concurrent.atomic.AtomicInteger(0);
    /** 上次成功 init 携带的 ext (用于检测网盘 cookie 变更并重建 spider) */
    private String lastInitExt;

    /**
     * 网盘账号在 WebView 登录后调用: 清空已缓存 spider 实例,
     * 下次 spider 调用会按最新的系统 CookieManager 重新 init。
     */
    public static void invalidateSpiders() {
        // 通过一个 static 引用访问服务单例的缓存 (服务是应用内唯一实例)
        BridgeService instance = getRunningInstance();
        if (instance == null) {
            return;
        }
        synchronized (instance.spiderCache) {
            instance.spiderCache.clear();
        }
        instance.initialized = false;
        instance.lastInitExt = null;
        Log.i(TAG, "spider 缓存已清空, 等待按新 CookieManager 重建");
    }

    private static BridgeService runningInstance;

    @Override
    public void onCreate() {
        super.onCreate();
        runningInstance = this;
    }

    private static BridgeService getRunningInstance() {
        return runningInstance;
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
            detailExecutor = Executors.newSingleThreadExecutor();
            new Thread(this::acceptLoop, "BridgeAccept").start();
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
    private String readLineRaw(java.io.InputStream in) throws Exception {
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

            // /detail 走优先通道: 用户点击播放不能排在全站搜索队列后面
            if ("/detail".equals(path) && "POST".equalsIgnoreCase(method)) {
                Log.i(TAG, method + " " + path + " body=" + bodyStr);
                java.net.Socket sock = s;
                detailExecutor.submit(() -> {
                    String r = doDetail(bodyStr);
                    try { writeHttp(sock, r); sock.close(); } catch (Exception e) { Log.e(TAG, "detail write: " + e); }
                });
                return;
            }

            Log.i(TAG, method + " " + path + " body=" + bodyStr);

            String resp;
            if ("/health".equals(path)) {
                resp = json(200, "ok", "{\"initialized\":" + initialized + "}");
            } else if ("/init".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doInit(bodyStr);
            } else if ("/search".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doSearch(bodyStr);
            } else if ("/playerContent".equals(path) && "POST".equalsIgnoreCase(method)) {
                // 播放二次解析: detailContent 的网盘资源 id → 真实直链 (走 detail 优先通道)
                Log.i(TAG, method + " " + path + " body=" + bodyStr);
                java.net.Socket sock = s;
                detailExecutor.submit(() -> {
                    String r = doPlayerContent(bodyStr);
                    try { writeHttp(sock, r); sock.close(); } catch (Exception e) { Log.e(TAG, "playerContent write: " + e); }
                });
                return;
            } else if ("/detail".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doDetail(bodyStr);
            } else if ("/home".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doHome(bodyStr);
            } else if ("/category".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doCategory(bodyStr);
            } else if ("/setCookie".equals(path) && "POST".equalsIgnoreCase(method)) {
                resp = doSetCookie(bodyStr);
            } else {
                resp = json(404, "not_found", null);
            }
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

    private String doInit(String body) {
        try {
            synchronized (spiderLock) {
                String ext = (body != null && !body.isEmpty()) ? parseField(body, "ext") : null;
                if (ext != null) extConfig = ext;
                if (initialized && (ext == null || ext.equals(lastInitExt))) {
                    // 已初始化且 ext 未变: 不重复跑 native 库检查与 Init.init
                    return json(200, null, "{\"ok\":true}");
                }
                // 首次初始化或 ext 变更(网盘 cookie 更新): 重建全部 spider 实例
                synchronized (spiderCache) {
                    spiderCache.clear();
                }
                ensureWexNativeLibs();
                Class<?> initCls = Class.forName("com.github.catvod.spider.Init");
                initCls.getMethod("init", Context.class).invoke(null, getApplication());
                initialized = true;
                lastInitExt = ext;
                Log.i(TAG, "init(重)完成, ext=" + (ext != null ? ext.length() + "字节" : "null"));
                return json(200, null, "{\"ok\":true}");
            }
        } catch (Throwable t) {
            Log.e(TAG, "init failed", t);
            return json(500, t.toString(), null);
        }
    }

    /**
     * wex 系 spider 依赖 files/TV/ 下的 native 库 (libLoadNiMa.so 等), 由原版宿主
     * 启动时下载。这里复刻该逻辑: api.txt(AES) → 配置服务器 → go.php(AES) → 按 ABI 下载。
     * 任一环节失败仅打日志, 不阻塞 spider 初始化 (非 wex 站点不需要这些库)。
     */
    private void ensureWexNativeLibs() {
        try {
            File tvDir = new File(getFilesDir(), "TV");
            if (!tvDir.exists()) tvDir.mkdirs();
            String conf = httpGetString(WEX_CONFIG_URL);
            if (conf == null) { Log.w(TAG, "wex conf fetch failed"); return; }
            String base = decryptWex(conf);
            if (base == null || base.isEmpty()) { Log.w(TAG, "wex conf decrypt failed"); return; }
            String goRaw = httpGetString(base + "/go.php");
            if (goRaw == null) { Log.w(TAG, "go.php fetch failed"); return; }
            String goJson = decryptWex(goRaw);
            if (goJson == null) { Log.w(TAG, "go.php decrypt failed"); return; }
            org.json.JSONObject go = new org.json.JSONObject(goJson);
            // libLoadNiMa.so → wex_* 条目; libdecjni.so → hxq_* 条目 (awenc 库)
            String key = abiKey();
            downloadIfSizeMismatch(tvDir, go, "libLoadNiMa.so", "wex_" + key + "_size", "wex_" + key + "_url");
            downloadIfSizeMismatch(tvDir, go, "libdecjni.so", "hxq_" + key + "_size", "hxq_" + key + "_url");
        } catch (Throwable t) {
            Log.w(TAG, "ensureWexNativeLibs: " + t);
        }
    }

    /** go.php 配置中对应当前设备 ABI 的 key 段 (v7/v8/x86/x86_64) */
    private String abiKey() {
        String abi = Build.SUPPORTED_ABIS.length > 0 ? Build.SUPPORTED_ABIS[0] : "";
        boolean isX86 = abi.startsWith("x86");
        if (abi.contains("64")) return isX86 ? "x86_64" : "v8";
        return isX86 ? "x86" : "v7";
    }

    /** 大小不符才下载 (go.php 提供 expected size) */
    private void downloadIfSizeMismatch(File tvDir, org.json.JSONObject go, String fileName, String sizeKey, String urlKey) {
        try {
            if (!go.has(urlKey)) { Log.w(TAG, "no url for " + urlKey); return; }
            long expected = go.getLong(sizeKey);
            File target = new File(tvDir, fileName);
            if (target.exists() && target.length() == expected) return;
            Log.i(TAG, "downloading " + fileName + " from " + go.getString(urlKey));
            byte[] data = httpGetBytes(go.getString(urlKey));
            if (data == null || data.length != expected) { Log.w(TAG, "size mismatch after download: " + (data == null ? -1 : data.length)); return; }
            java.io.FileOutputStream fos = new java.io.FileOutputStream(target);
            fos.write(data);
            fos.close();
            target.setReadable(true, false);
            target.setExecutable(true, false);
            Log.i(TAG, "saved " + fileName + " (" + data.length + " bytes)");
        } catch (Throwable t) {
            Log.w(TAG, "downloadIfSizeMismatch " + fileName + ": " + t);
        }
    }

    /** AES/CBC/PKCS7 解密 wex 配置 (key/iv 为固定混淆串) */
    private String decryptWex(String base64) {
        try {
            byte[] ct = android.util.Base64.decode(base64.trim(), android.util.Base64.DEFAULT);
            javax.crypto.Cipher cipher = javax.crypto.Cipher.getInstance("AES/CBC/PKCS5Padding");
            cipher.init(javax.crypto.Cipher.DECRYPT_MODE,
                new javax.crypto.spec.SecretKeySpec("nifanbianyikeyia".getBytes("UTF-8"), "AES"),
                new javax.crypto.spec.IvParameterSpec("keyijiangjiudian".getBytes("UTF-8")));
            return new String(cipher.doFinal(ct), "UTF-8").trim();
        } catch (Throwable t) {
            Log.w(TAG, "decryptWex: " + t);
            return null;
        }
    }

    private String httpGetString(String url) {
        byte[] data = httpGetBytes(url);
        return data == null ? null : new String(data, java.nio.charset.StandardCharsets.UTF_8);
    }

    private byte[] httpGetBytes(String url) {
        java.net.HttpURLConnection conn = null;
        try {
            conn = (java.net.HttpURLConnection) new java.net.URL(url).openConnection();
            conn.setConnectTimeout(10000);
            conn.setReadTimeout(20000);
            conn.setRequestProperty("User-Agent", "okhttp/4.12.0");
            java.io.InputStream in = conn.getResponseCode() >= 400 ? conn.getErrorStream() : conn.getInputStream();
            java.io.ByteArrayOutputStream bos = new java.io.ByteArrayOutputStream();
            byte[] buf = new byte[8192];
            int n;
            while ((n = in.read(buf)) != -1) bos.write(buf, 0, n);
            in.close();
            return bos.toByteArray();
        } catch (Throwable t) {
            Log.w(TAG, "httpGet " + url + ": " + t);
            return null;
        } finally {
            if (conn != null) conn.disconnect();
        }
    }

    private String doSearch(String body) {
        try {
            String className = parseField(body, "class");
            String keyword = parseField(body, "keyword");
            if (className == null || keyword == null) return json(400, "missing class/keyword", null);
            return invokeSpiderWithRetry(className, "searchContent", new Class[]{String.class, boolean.class}, new Object[]{keyword, true}, false);
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    private String doDetail(String body) {
        try {
            String className = parseField(body, "class");
            String ids = parseField(body, "ids");
            if (className == null || ids == null) return json(400, "missing class/ids", null);
            return invokeSpiderWithRetry(className, "detailContent", new Class[]{List.class}, new Object[]{java.util.Arrays.asList(ids.split(","))}, true);
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    /**
     * spider 500 时重建实例重试一次。
     * 场景: wex 系 spider 的运行时站点配置 (CDN 多 IP, 模拟器 DNS 轮询可能拿到死 IP)
     * 拉取失败后进程内缓存 null, 后续 detail/category 持续 NPE; 重建实例触发重新拉取。
     * 实测: 重启桥接进程即恢复 → 等价的实例级重建 + 单次重试。
     */
    private String invokeSpiderWithRetry(String className, String method, Class<?>[] paramTypes, Object[] args, boolean priority) {
        String r = invokeSpider(className, method, paramTypes, args, priority);
        if (r != null && r.contains("\"code\":500")) {
            Log.w(TAG, method + " 500, 重建 spider 实例后重试一次");
            invalidateSpiders();
            r = invokeSpider(className, method, paramTypes, args, priority);
        }
        return r;
    }

    private String doPlayerContent(String body) {
        try {
            String className = parseField(body, "class");
            String flag = parseField(body, "flag");
            String id = parseField(body, "id");
            if (className == null || id == null) return json(400, "missing class/id", null);
            if (flag == null) flag = "";
            // TVBox 标准: playerContent(flag, id, vipFlags) — 三参签名优先, 两参变体兜底
            String r3 = null;
            try {
                r3 = invokeSpider(className, "playerContent",
                    new Class[]{String.class, String.class, java.util.List.class},
                    new Object[]{flag, id, java.util.Collections.emptyList()}, true);
            } catch (Throwable t3) {
                Log.w(TAG, "3-arg playerContent failed: " + t3);
            }
            if (r3 != null && !r3.contains("NoSuchMethodException")) return r3;
            return invokeSpiderWithRetry(className, "playerContent",
                new Class[]{String.class, String.class},
                new Object[]{flag, id}, true);
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    private String doHome(String body) {
        try {
            String className = parseField(body, "class");
            if (className == null) return json(400, "missing class", null);
            return invokeSpider(className, "homeContent", new Class[]{boolean.class}, new Object[]{true});
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    private String doCategory(String body) {
        try {
            String className = parseField(body, "class");
            String tid = parseField(body, "tid");
            String pg = parseField(body, "pg");
            if (className == null) return json(400, "missing class", null);
            if (tid == null) tid = "1";
            if (pg == null) pg = "1";
            return invokeSpiderWithRetry(className, "categoryContent",
                new Class[]{String.class, String.class, boolean.class, java.util.HashMap.class},
                new Object[]{tid, pg, true, new java.util.HashMap<String, String>()}, true);
        } catch (Throwable t) {
            return json(500, t.toString(), null);
        }
    }

    private String invokeSpider(String className, String method, Class<?>[] paramTypes, Object[] args) {
        return invokeSpider(className, method, paramTypes, args, false);
    }

    /**
     * spider 调用统一入口, native 链不支持并发故全互斥。
     * @param priority detail 用: 进入锁前置位, 让正在搜索的线程在片段边界主动让出锁
     */
    private String invokeSpider(String className, String method, Class<?>[] paramTypes, Object[] args, boolean priority) {
        if (priority) detailPending.incrementAndGet();
        try {
            synchronized (spiderLock) {
                if (priority) detailPending.decrementAndGet();
                // 短名自动补全包名 (桌面端传 csp_ 剥离后的短名)
                String fqn = className.indexOf('.') >= 0 ? className : "com.github.catvod.spider." + className;
                Class<?> initCls = Class.forName("com.github.catvod.spider.Init");
                if (!initialized) {
                    // 自愈: /search 直接到达(未经 /init)时也要确保 wex native 库就位
                    ensureWexNativeLibs();
                    initCls.getMethod("init", Context.class).invoke(null, getApplication());
                    initialized = true;
                }
                Method getSpider = initCls.getMethod("getSpider", String.class);
                Object spider;
                synchronized (spiderCache) {
                    spider = spiderCache.get(fqn);
                    if (spider == null) {
                        spider = getSpider.invoke(null, fqn);
                        if (spider == null) return json(404, "spider not found: " + fqn, null);
                        // TVBoxOSC 流程: newInstance 后必须调用 init(context, ext), 否则部分
                        // spider (如 wex 系) 的资源初始化(libLoadNiMa.so 提取)不会执行
                        try {
                            java.lang.reflect.Field keyField = findField(spider.getClass(), "siteKey");
                            if (keyField != null) {
                                keyField.setAccessible(true);
                                keyField.set(spider, className);
                            }
                        } catch (Throwable ignored) {
                        }
                        try {
                            spider.getClass()
                                .getMethod("init", Context.class, String.class)
                                .invoke(spider, getApplication(), extConfig);
                        } catch (NoSuchMethodException e) {
                            spider.getClass().getMethod("init", Context.class).invoke(spider, getApplication());
                        }
                        spiderCache.put(fqn, spider);
                    }
                }
                Method m = spider.getClass().getMethod(method, paramTypes);
                Object result;
                if (priority) {
                    // detail 的真正网络请求不占 spiderLock: 初始化/取类已在此锁内完成,
                    // 多数 spider 的 detailContent 只是单次 HTTP + 解析, 原子性风险远低于搜索
                    result = m.invoke(spider, args);
                } else {
                    // 搜索: 调用前后检查 detail 优先标记, 让在途 detail 尽快插进下个空档
                    result = m.invoke(spider, args);
                    while (detailPending.get() > 0) {
                        synchronized (spiderLock) {
                            spiderLock.wait(50);
                        }
                    }
                }
                String data = result == null ? "null" : result.toString();
                return json(200, null, "\"" + esc(data) + "\"");
            }
        } catch (Throwable t) {
            Log.e(TAG, "invokeSpider failed", t);
            return json(500, t.toString(), null);
        }
    }

    /** 反射向上查找字段 (wex 系 Spider 基类有 public siteKey 字段) */
    private static java.lang.reflect.Field findField(Class<?> clz, String name) {
        for (Class<?> c = clz; c != null; c = c.getSuperclass()) {
            try {
                return c.getDeclaredField(name);
            } catch (NoSuchFieldException ignored) {
            }
        }
        return null;
    }

    /** 桌面端扫码登录: cookie 写入 CookieManager + 落盘, 并重建 spider 实例 */
    private String doSetCookie(String body) {
        try {
            String drive = parseField(body, "drive");
            String cookie = parseField(body, "cookie");
            if (drive == null || cookie == null || cookie.isEmpty()) {
                return json(400, "missing drive/cookie", null);
            }
            String[] hosts = cookieHosts(drive);
            if (hosts == null) {
                return json(400, "unsupported drive: " + drive, null);
            }
            android.webkit.CookieManager cm = android.webkit.CookieManager.getInstance();
            cm.setAcceptCookie(true);
            for (String h : hosts) {
                cm.setCookie("https://" + h + "/", cookie);
            }
            cm.flush();
            writeCookieFile(this, drive, cookie);
            invalidateSpiders();
            Log.i(TAG, "setCookie: drive=" + drive + " len=" + cookie.length());
            return json(200, "ok", null);
        } catch (Exception e) {
            Log.e(TAG, "setCookie", e);
            return json(500, "set_cookie_failed", null);
        }
    }

    /** 各网盘登录态所在域 (与 spider 读取通道一致) */
    private static String[] cookieHosts(String drive) {
        switch (drive) {
            case "quark": return new String[]{"pan.quark.cn", "quark.cn", "uop.quark.cn", "drive-pc.quark.cn"};
            case "uc":    return new String[]{"drive.uc.cn", "uc.cn", "pc.uc.cn"};
            case "baidu": return new String[]{"pan.baidu.com", "passport.baidu.com", "wappass.baidu.com"};
            default: return null;
        }
    }

    /** 登录 cookie 落盘到 files/TV/.<drive>cookie (部分 spider 读文件兜底) */
    static void writeCookieFile(android.content.Context ctx, String drive, String cookies) {
        try {
            java.io.File dir = new java.io.File(ctx.getFilesDir(), "TV");
            if (!dir.exists()) dir.mkdirs();
            java.io.File f = new java.io.File(dir, "." + drive + "cookie");
            java.io.FileOutputStream fos = new java.io.FileOutputStream(f, false);
            if (cookies != null) fos.write(cookies.getBytes(java.nio.charset.StandardCharsets.UTF_8));
            fos.flush();
            fos.close();
            f.setReadable(true, false);
            Log.i("CloudLogin", "cookie 已写入: " + f.getAbsolutePath());
        } catch (Exception e) {
            Log.e("CloudLogin", "persist cookie file failed", e);
        }
    }

    private String parseField(String body, String key) {
        // very minimal JSON parse for string fields
        String pat = "\"" + key + "\":\"";
        int i = body.indexOf(pat);
        if (i < 0) return null;
        int s = i + pat.length();
        int e = body.indexOf("\"", s);
        if (e < 0) return null;
        return body.substring(s, e).replace("\\\\", "\\").replace("\\\"", "\"");
    }

    @Override
    public void onDestroy() {
        if (runningInstance == this) runningInstance = null;
        try { if (server != null) server.close(); } catch (Exception e) {}
        super.onDestroy();
    }
}
