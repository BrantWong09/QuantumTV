import java.io.File;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.net.URL;
import java.net.URLClassLoader;
import java.util.ArrayList;
import java.util.Collection;
import java.util.List;
import java.util.Map;

public class SpiderRunner {

    public static void main(String[] args) {
        String empty = "{\"list\":[]}";
        if (args.length < 3) {
            System.out.println(empty);
            return;
        }
        String jarPath = args[0];
        String className = args[1];
        String query = args[2];

        try {
            URLClassLoader loader = new URLClassLoader(
                    new URL[] {new File(jarPath).toURI().toURL()},
                    SpiderRunner.class.getClassLoader());
            Class<?> clazz = Class.forName(className, true, loader);
            Object spider = clazz.getDeclaredConstructor().newInstance();

            tryInit(spider);

            Object result = doSearch(spider, query);
            String json = toJson(result, loader);
            if (json == null) {
                System.out.println(empty);
            } else {
                System.out.println(json);
            }
        } catch (Throwable t) {
            System.out.println(empty);
        }
    }

    private static void tryInit(Object spider) {
        for (Method m : spider.getClass().getMethods()) {
            if (!"init".equals(m.getName())) {
                continue;
            }
            try {
                Object[] args = new Object[m.getParameterCount()];
                m.setAccessible(true);
                m.invoke(spider, args);
                return;
            } catch (Exception ignored) {
            }
        }
    }

    private static Object doSearch(Object spider, String query) {
        Method m = findMethod(spider, "searchContent", 2);
        if (m != null) {
            try {
                return m.invoke(spider, query, true);
            } catch (Exception ignored) {
            }
        }
        m = findMethod(spider, "searchContent", 1);
        if (m != null) {
            try {
                return m.invoke(spider, query);
            } catch (Exception ignored) {
            }
        }
        return null;
    }

    private static Method findMethod(Object obj, String name, int paramCount) {
        for (Method m : obj.getClass().getMethods()) {
            if (name.equals(m.getName()) && m.getParameterCount() == paramCount) {
                return m;
            }
        }
        return null;
    }

    private static String toJson(Object result, ClassLoader loader) {
        if (result == null) {
            return null;
        }
        if (result instanceof Collection || result.getClass().isArray()) {
            List<Object> list = new ArrayList<>();
            if (result instanceof Collection) {
                list.addAll((Collection<?>) result);
            } else {
                int len = java.lang.reflect.Array.getLength(result);
                for (int i = 0; i < len; i++) {
                    list.add(java.lang.reflect.Array.get(result, i));
                }
            }
            return "{\"list\":" + jsonArray(list, loader) + "}";
        }
        String fastjson = fastJson(result, loader);
        if (fastjson != null) {
            return fastjson;
        }
        return manualJson(result);
    }

    private static String fastJson(Object obj, ClassLoader loader) {
        try {
            Class<?> jsonClass = Class.forName("com.alibaba.fastjson.JSON", true, loader);
            Method m = jsonClass.getMethod("toJSONString", Object.class);
            Object out = m.invoke(null, obj);
            if (out != null) {
                String s = out.toString();
                if (s.startsWith("{") || s.startsWith("[")) {
                    return s;
                }
            }
        } catch (Exception ignored) {
        }
        return null;
    }

    private static String manualJson(Object obj) {
        if (obj == null) {
            return "null";
        }
        if (obj instanceof String) {
            return "\"" + escape((String) obj) + "\"";
        }
        if (obj instanceof Number || obj instanceof Boolean || obj instanceof Character) {
            return String.valueOf(obj);
        }
        if (obj instanceof Collection) {
            return jsonArray(new ArrayList<>((Collection<?>) obj), obj.getClass().getClassLoader());
        }
        if (obj.getClass().isArray()) {
            int len = java.lang.reflect.Array.getLength(obj);
            List<Object> list = new ArrayList<>();
            for (int i = 0; i < len; i++) {
                list.add(java.lang.reflect.Array.get(obj, i));
            }
            return jsonArray(list, obj.getClass().getClassLoader());
        }
        if (obj instanceof Map) {
            StringBuilder mb = new StringBuilder("{");
            boolean mfirst = true;
            for (Map.Entry<?, ?> e : ((Map<?, ?>) obj).entrySet()) {
                if (!mfirst) {
                    mb.append(",");
                }
                mfirst = false;
                mb.append("\"").append(escape(String.valueOf(e.getKey()))).append("\":")
                        .append(manualJson(e.getValue()));
            }
            mb.append("}");
            return mb.toString();
        }
        // 普通对象: 序列化公共字段 + getter
        StringBuilder sb = new StringBuilder("{");
        boolean first = true;
        try {
            for (Field f : obj.getClass().getFields()) {
                if (java.lang.reflect.Modifier.isStatic(f.getModifiers())) {
                    continue;
                }
                Object val = f.get(obj);
                if (!first) {
                    sb.append(",");
                }
                first = false;
                sb.append("\"").append(escape(f.getName())).append("\":").append(manualJson(val));
            }
        } catch (Exception ignored) {
        }
        try {
            for (Method m : obj.getClass().getMethods()) {
                String name = m.getName();
                if (m.getParameterCount() == 0 && name.length() > 3 && name.startsWith("get")) {
                    Class<?> rt = m.getReturnType();
                    if (rt == void.class || rt == Class.class || rt == ClassLoader.class) {
                        continue;
                    }
                    String field = Character.toLowerCase(name.charAt(3)) + name.substring(4);
                    Object val = m.invoke(obj);
                    if (val == null) {
                        continue;
                    }
                    if (!first) {
                        sb.append(",");
                    }
                    first = false;
                    sb.append("\"").append(escape(field)).append("\":").append(manualJson(val));
                }
            }
        } catch (Exception ignored) {
        }
        sb.append("}");
        return sb.toString();
    }

    private static String jsonArray(List<Object> items, ClassLoader loader) {
        StringBuilder sb = new StringBuilder("[");
        boolean first = true;
        for (Object item : items) {
            if (!first) {
                sb.append(",");
            }
            first = false;
            String s = fastJson(item, loader);
            sb.append(s != null ? s : manualJson(item));
        }
        sb.append("]");
        return sb.toString();
    }

    private static String escape(String s) {
        return s.replace("\\", "\\\\")
                .replace("\"", "\\\"")
                .replace("\n", "\\n")
                .replace("\r", "\\r")
                .replace("\t", "\\t");
    }
}
