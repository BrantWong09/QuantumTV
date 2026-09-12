package com.quantumtv.bridge.ipc;

/** 极简 JSON 字符串字段提取/转义 (桌面↔control↔worker 三层共用的小载荷格式)。纯 Java, host 可测。 */
public final class JsonLite {
    private JsonLite() {}

    /** 取 "key":"value" 的 value (反转义 \" \\ \n \r \t); 不存在返回 null */
    public static String string(String json, String key) {
        String pat = "\"" + key + "\":\"";
        int i = json.indexOf(pat);
        if (i < 0) return null;
        StringBuilder sb = new StringBuilder();
        for (int p = i + pat.length(); p < json.length(); p++) {
            char c = json.charAt(p);
            if (c == '"') return sb.toString();
            if (c == '\\' && p + 1 < json.length()) {
                char n = json.charAt(++p);
                switch (n) {
                    case 'n': sb.append('\n'); break;
                    case 'r': sb.append('\r'); break;
                    case 't': sb.append('\t'); break;
                    default: sb.append(n);
                }
                continue;
            }
            sb.append(c);
        }
        return null;
    }

    public static String escape(String s) {
        StringBuilder sb = new StringBuilder(s.length() + 16);
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            switch (c) {
                case '"': sb.append("\\\""); break;
                case '\\': sb.append("\\\\"); break;
                case '\n': sb.append("\\n"); break;
                case '\r': sb.append("\\r"); break;
                case '\t': sb.append("\\t"); break;
                default: sb.append(c);
            }
        }
        return sb.toString();
    }

    /** 包装为 JSON 字符串字面量 ("escaped") */
    public static String quote(String s) {
        return "\"" + escape(s) + "\"";
    }
}
