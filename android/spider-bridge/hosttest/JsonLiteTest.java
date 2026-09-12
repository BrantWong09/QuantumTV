import com.quantumtv.bridge.ipc.JsonLite;

public class JsonLiteTest {
    static void eq(Object a, Object b, String m) {
        if (!java.util.Objects.equals(a, b)) throw new AssertionError(m + ": " + a + " != " + b);
    }
    public static void main(String[] a) {
        eq(JsonLite.string("{\"method\":\"search\",\"keyword\":\"夜王\"}", "keyword"), "夜王", "plain");
        eq(JsonLite.string("{\"id\":\"a\\\"b\"}", "id"), "a\"b", "escaped quote in value");
        eq(JsonLite.string("{\"d\":\"x\\\\y\"}", "d"), "x\\y", "escaped backslash");
        eq(JsonLite.string("{\"m\":\"a\\nb\"}", "m"), "a\nb", "newline");
        eq(JsonLite.string("{\"no\":1}", "no"), null, "non-string value → null");
        eq(JsonLite.string("{}", "missing"), null, "missing key");
        eq(JsonLite.quote("a\"b"), "\"a\\\"b\"", "quote");
        // 往返
        String raw = "line1\nline2\t\"quoted\" \\path\\";
        eq(JsonLite.string("{\"k\":" + JsonLite.quote(raw) + "}", "k"), raw, "escape→string 往返");
        System.out.println("JsonLiteTest OK");
    }
}
