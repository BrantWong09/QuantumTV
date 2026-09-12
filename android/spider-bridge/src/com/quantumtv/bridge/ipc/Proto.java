package com.quantumtv.bridge.ipc;

/** Control↔Worker IPC 帧: [u32 BE reqId][u32 BE type][u32 BE len][payload] (与隧道帧同风格, 方案 §8)。
 *  纯 Java 零 android 依赖, host JVM 直接跑单测。 */
public final class Proto {
    public static final int T_HELLO = 1;  // → {role,pid}
    public static final int T_READY = 2;  // → {}
    public static final int T_REQ   = 3;  // → {method,class,args...}
    public static final int T_RESP  = 4;  // → {reqId,code,err,data}
    public static final int T_HB    = 5;  // → {state,curReqId,curMethod,curAgeMs}
    public static final int T_COOKIE = 6; // → {ext,drives}
    public static final int T_HANG  = 7;  // debug: 永久挂起当前执行线程 (隔离验收钩子)
    public static final int MAX_PAYLOAD = 8 * 1024 * 1024;

    public static final class Frame {
        public final int reqId;
        public final int type;
        public final byte[] payload;
        public final int used;
        Frame(int reqId, int type, byte[] payload, int used) {
            this.reqId = reqId; this.type = type; this.payload = payload; this.used = used;
        }
    }

    private Proto() {}

    public static byte[] encode(int reqId, int type, byte[] payload) {
        if (payload.length > MAX_PAYLOAD) {
            throw new IllegalArgumentException("payload too large: " + payload.length);
        }
        byte[] out = new byte[12 + payload.length];
        writeInt(out, 0, reqId);
        writeInt(out, 4, type);
        writeInt(out, 8, payload.length);
        System.arraycopy(payload, 0, out, 12, payload.length);
        return out;
    }

    /** 从 buf[off..off+len) 解一帧; 不完整或畸形返回 null (调用方断开该链路) */
    public static Frame decode(byte[] buf, int off, int len) {
        if (len < 12) return null;
        int reqId = readInt(buf, off);
        int type = readInt(buf, off + 4);
        int plen = readInt(buf, off + 8);
        if (plen < 0 || plen > MAX_PAYLOAD) return null;
        if (len - 12 < plen) return null;
        byte[] payload = new byte[plen];
        System.arraycopy(buf, off + 12, payload, 0, plen);
        return new Frame(reqId, type, payload, 12 + plen);
    }

    static void writeInt(byte[] b, int o, int v) {
        b[o] = (byte) (v >>> 24); b[o + 1] = (byte) (v >>> 16);
        b[o + 2] = (byte) (v >>> 8); b[o + 3] = (byte) v;
    }

    static int readInt(byte[] b, int o) {
        return ((b[o] & 0xFF) << 24) | ((b[o + 1] & 0xFF) << 16)
             | ((b[o + 2] & 0xFF) << 8) | (b[o + 3] & 0xFF);
    }
}
