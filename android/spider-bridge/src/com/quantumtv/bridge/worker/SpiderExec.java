package com.quantumtv.bridge.worker;

import android.content.Context;
import android.os.Build;
import android.util.Base64;
import android.util.Log;
import android.webkit.CookieManager;

import com.quantumtv.bridge.ipc.JsonLite;

import java.io.File;
import java.io.FileOutputStream;
import java.lang.reflect.Method;
import java.util.Collections;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * Worker 进程内的 spider 执行体 (方案 §7/§27-§29/§57): 从 BridgeService 整体迁入。
 * 关键差异: 无全局 spiderLock —— 进程隔离即串行 (1 process = 1 spider = 1 active call);
 * 卡死的唯一出路是 Control Watchdog killProcess (§21), 不再有"持锁僵尸拖死全家"。
 * 依赖仅 android.* + 反射加载的 catvod spider (与旧实现同一份 jar/资产)。
 */
public final class SpiderExec {
    private static final String TAG = "BridgeWorker";
    private static final String WEX_CONFIG_URL = "https://9280.kstore.vip/api.txt";

    public static final class Resp {
        public final int code;
        public final String err;
        public final String data;
        Resp(int c, String e, String d) { code = c; err = e; data = d; }
    }

    private static final SpiderExec INSTANCE = new SpiderExec();
    public static SpiderExec get() { return INSTANCE; }

    private Context app;
    private volatile String extConfig = "";
    private volatile boolean initialized = false;
    private volatile String lastInitExt;
    /** 已初始化的 spider 实例缓存: 单线程访问, 无需锁 (§57) */
    private final Map<String, Object> spiderCache = new HashMap<>();

    public void attach(Context app, String ext) {
        this.app = app;
        this.extConfig = ext == null ? "" : ext;
        restoreCookieFiles();
    }

    /**
     * 进程隔离回归修复: spider 读的是本进程 webkit CookieManager, 而登录 cookie 由主进程
     * CloudLoginActivity 写入其独立 jar; 文件通道 files/TV/.<drive>cookie 是唯一跨进程真相源
     * (桌面从不重推, control cookieStore 重启即空)。worker 启动时据此回填本进程。
     */
    public void restoreCookieFiles() {
        if (app == null) return;
        try {
            CookieManager cm = CookieManager.getInstance();
            cm.setAcceptCookie(true);
            for (String drive : new String[]{"quark", "uc", "baidu"}) {
                File f = new File(new File(app.getFilesDir(), "TV"), "." + drive + "cookie");
                if (!f.exists() || f.length() == 0) continue;
                byte[] buf = new byte[(int) f.length()];
                java.io.FileInputStream fis = new java.io.FileInputStream(f);
                int n = fis.read(buf);
                fis.close();
                if (n <= 0) continue;
                String cookie = new String(buf, 0, n, "UTF-8").trim();
                if (cookie.isEmpty()) continue;
                for (String h : cookieHosts(drive)) cm.setCookie("https://" + h + "/", cookie);
                Log.i(TAG, "restoreCookieFiles: " + drive + " " + cookie.length() + "B 已注入本进程");
            }
            cm.flush();
        } catch (Throwable t) {
            Log.w(TAG, "restoreCookieFiles: " + t);
        }
    }

    /** 网盘 cookie/ext 变更: 清实例缓存, 下次调用按最新 CookieManager/文件重建 (§28 下发通道) */
    public synchronized void reinit(String ext) {
        if (ext != null) extConfig = ext;
        spiderCache.clear();
        initialized = false;
        lastInitExt = null;
        Log.i(TAG, "spider 缓存已清空, 按新配置重建");
    }

    /** 控制面下发的 cookie: 写本进程 CookieManager + 文件兜底 + 重建实例 (方案 §决策#2) */
    public synchronized void applyCookie(String drive, String cookie) {
        String[] hosts = cookieHosts(drive);
        if (hosts == null || cookie == null || cookie.isEmpty()) return;
        try {
            CookieManager cm = CookieManager.getInstance();
            cm.setAcceptCookie(true);
            for (String h : hosts) cm.setCookie("https://" + h + "/", cookie);
            cm.flush();
        } catch (Throwable t) {
            Log.w(TAG, "applyCookie: " + t);
        }
        writeCookieFile(app, drive, cookie);
        reinit(null);
    }

    public static String[] cookieHosts(String drive) {
        switch (drive) {
            case "quark": return new String[]{"pan.quark.cn", "quark.cn", "uop.quark.cn", "drive-pc.quark.cn"};
            case "uc":    return new String[]{"drive.uc.cn", "uc.cn", "pc.uc.cn"};
            case "baidu": return new String[]{"pan.baidu.com", "passport.baidu.com", "wappass.baidu.com"};
            default: return null;
        }
    }

    /** 登录 cookie 落盘到 files/TV/.<drive>cookie (spider 文件兜底通道, 跨进程共享同一 data 目录) */
    public static void writeCookieFile(Context ctx, String drive, String cookies) {
        try {
            File dir = new File(ctx.getFilesDir(), "TV");
            if (!dir.exists()) dir.mkdirs();
            File f = new File(dir, "." + drive + "cookie");
            FileOutputStream fos = new FileOutputStream(f, false);
            if (cookies != null) fos.write(cookies.getBytes("UTF-8"));
            fos.flush();
            fos.close();
            f.setReadable(true, false);
        } catch (Exception e) {
            Log.e(TAG, "persist cookie file failed", e);
        }
    }

    /** REQ 执行入口 (T6): method ∈ {search,detail,playerContent,home,category} */
    public Resp invoke(String method, String json) {
        try {
            String cls = JsonLite.string(json, "class");
            if (cls == null) return new Resp(400, "missing class", null);
            switch (method) {
                case "search": {
                    String kw = JsonLite.string(json, "keyword");
                    if (kw == null) return new Resp(400, "missing keyword", null);
                    return invokeWithRetry(cls, "searchContent",
                            new Class<?>[]{String.class, boolean.class}, new Object[]{kw, true});
                }
                case "detail": {
                    String ids = JsonLite.string(json, "ids");
                    if (ids == null) return new Resp(400, "missing ids", null);
                    return invokeWithRetry(cls, "detailContent",
                            new Class<?>[]{List.class}, new Object[]{java.util.Arrays.asList(ids.split(","))});
                }
                case "playerContent": {
                    String flag = JsonLite.string(json, "flag");
                    String id = JsonLite.string(json, "id");
                    if (id == null) return new Resp(400, "missing id", null);
                    if (flag == null) flag = "";
                    // TVBox 标准三参优先, 两参兜底 (与旧 doPlayerContent 一致)
                    try {
                        Resp r3 = invokeSpider(cls, "playerContent",
                                new Class<?>[]{String.class, String.class, List.class},
                                new Object[]{flag, id, Collections.emptyList()});
                        if (r3.code == 200 || !String.valueOf(r3.err).contains("NoSuchMethodException")) return r3;
                    } catch (Throwable t3) {
                        Log.w(TAG, "3-arg playerContent failed: " + t3);
                    }
                    return invokeWithRetry(cls, "playerContent",
                            new Class<?>[]{String.class, String.class}, new Object[]{flag, id});
                }
                case "home":
                    return invokeSpider(cls, "homeContent", new Class<?>[]{boolean.class}, new Object[]{true});
                case "category": {
                    String tid = JsonLite.string(json, "tid");
                    String pg = JsonLite.string(json, "pg");
                    if (tid == null) tid = "1";
                    if (pg == null) pg = "1";
                    return invokeWithRetry(cls, "categoryContent",
                            new Class<?>[]{String.class, String.class, boolean.class, HashMap.class},
                            new Object[]{tid, pg, true, new HashMap<String, String>()});
                }
                default:
                    return new Resp(404, "unknown method: " + method, null);
            }
        } catch (Throwable t) {
            Log.e(TAG, "invoke " + method, t);
            return new Resp(500, String.valueOf(t), null);
        }
    }

    /** spider 500 时重建实例重试一次 (旧 invokeSpiderWithRetry 语义原样保留) */
    private Resp invokeWithRetry(String cls, String method, Class<?>[] pt, Object[] args) throws Throwable {
        Resp r = invokeSpider(cls, method, pt, args);
        if (r.code == 500) {
            Log.w(TAG, method + " 500, 重建 spider 实例后重试一次");
            reinit(null);
            r = invokeSpider(cls, method, pt, args);
        }
        return r;
    }

    private Resp invokeSpider(String className, String method, Class<?>[] paramTypes, Object[] args) throws Throwable {
        ensureInit();
        String fqn = className.indexOf('.') >= 0 ? className : "com.github.catvod.spider." + className;
        Class<?> initCls = Class.forName("com.github.catvod.spider.Init");
        Method getSpider = initCls.getMethod("getSpider", String.class);
        Object spider = spiderCache.get(fqn);
        if (spider == null) {
            spider = getSpider.invoke(null, fqn);
            if (spider == null) return new Resp(404, "spider not found: " + fqn, null);
            try {
                java.lang.reflect.Field keyField = findField(spider.getClass(), "siteKey");
                if (keyField != null) {
                    keyField.setAccessible(true);
                    keyField.set(spider, className);
                }
            } catch (Throwable ignored) { }
            try {
                spider.getClass().getMethod("init", Context.class, String.class)
                        .invoke(spider, app, extConfig);
            } catch (NoSuchMethodException e) {
                spider.getClass().getMethod("init", Context.class).invoke(spider, app);
            }
            spiderCache.put(fqn, spider);
        }
        Method m = spider.getClass().getMethod(method, paramTypes);
        Object result;
        try {
            result = m.invoke(spider, args);
        } catch (java.lang.reflect.InvocationTargetException ite) {
            throw ite.getCause() == null ? ite : ite.getCause();
        }
        return new Resp(200, null, result == null ? "null" : result.toString());
    }

    private void ensureInit() throws Exception {
        if (initialized) return;
        ensureWexNativeLibs();
        Class<?> initCls = Class.forName("com.github.catvod.spider.Init");
        initCls.getMethod("init", Context.class).invoke(null, app);
        initialized = true;
    }

    private static java.lang.reflect.Field findField(Class<?> clz, String name) {
        for (Class<?> c = clz; c != null; c = c.getSuperclass()) {
            try {
                return c.getDeclaredField(name);
            } catch (NoSuchFieldException ignored) { }
        }
        return null;
    }

    // ---- wex native 库自举 (旧 BridgeService 原样迁移: api.txt→go.php→按 ABI 下载) ----

    private void ensureWexNativeLibs() {
        try {
            File tvDir = new File(app.getFilesDir(), "TV");
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
            String key = abiKey();
            downloadIfSizeMismatch(tvDir, go, "libLoadNiMa.so", "wex_" + key + "_size", "wex_" + key + "_url");
            downloadIfSizeMismatch(tvDir, go, "libdecjni.so", "hxq_" + key + "_size", "hxq_" + key + "_url");
        } catch (Throwable t) {
            Log.w(TAG, "ensureWexNativeLibs: " + t);
        }
    }

    private String abiKey() {
        String abi = Build.SUPPORTED_ABIS.length > 0 ? Build.SUPPORTED_ABIS[0] : "";
        boolean isX86 = abi.startsWith("x86");
        if (abi.contains("64")) return isX86 ? "x86_64" : "v8";
        return isX86 ? "x86" : "v7";
    }

    private void downloadIfSizeMismatch(File tvDir, org.json.JSONObject go, String fileName, String sizeKey, String urlKey) {
        try {
            if (!go.has(urlKey)) { Log.w(TAG, "no url for " + urlKey); return; }
            long expected = go.getLong(sizeKey);
            File target = new File(tvDir, fileName);
            if (target.exists() && target.length() == expected) return;
            Log.i(TAG, "downloading " + fileName);
            byte[] data = httpGetBytes(go.getString(urlKey));
            if (data == null || data.length != expected) {
                Log.w(TAG, "size mismatch after download: " + (data == null ? -1 : data.length));
                return;
            }
            FileOutputStream fos = new FileOutputStream(target);
            fos.write(data);
            fos.close();
            target.setReadable(true, false);
            target.setExecutable(true, false);
        } catch (Throwable t) {
            Log.w(TAG, "downloadIfSizeMismatch " + fileName + ": " + t);
        }
    }

    private String decryptWex(String base64) {
        try {
            byte[] ct = Base64.decode(base64.trim(), Base64.DEFAULT);
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
        try { return data == null ? null : new String(data, "UTF-8"); }
        catch (Exception e) { return null; }
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
}
