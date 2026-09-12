import com.quantumtv.bridge.ipc.Proto;
import java.util.Arrays;

public class ProtoTest {
    static void eq(Object a, Object b, String msg) {
        if (!java.util.Objects.equals(a, b)) throw new AssertionError(msg + ": " + a + " != " + b);
    }
    public static void main(String[] args) throws Exception {
        // encode→decode 往返
        byte[] raw = Proto.encode(7, Proto.T_REQ, "{\"m\":\"search\"}".getBytes("UTF-8"));
        eq(raw[0], (byte) 0, "id BE 0");
        eq(raw[3], (byte) 7, "id BE 3");
        Proto.Frame f = Proto.decode(raw, 0, raw.length);
        eq(f.reqId, 7, "reqId");
        eq(f.type, Proto.T_REQ, "type");
        eq(new String(f.payload, "UTF-8"), "{\"m\":\"search\"}", "payload");

        // 不完整返回 null
        byte[] part = Arrays.copyOf(raw, raw.length - 3);
        eq(Proto.decode(part, 0, part.length), null, "partial must be null");

        // 多帧连读 (流式粘包)
        byte[] a = Proto.encode(1, Proto.T_HB, new byte[]{1});
        byte[] b = Proto.encode(2, Proto.T_RESP, new byte[]{2, 3});
        byte[] both = new byte[a.length + b.length];
        System.arraycopy(a, 0, both, 0, a.length);
        System.arraycopy(b, 0, both, a.length, b.length);
        Proto.Frame x = Proto.decode(both, 0, both.length);
        eq(x.reqId, 1, "frame1 id");
        eq(x.used, a.length, "frame1 used");
        Proto.Frame y = Proto.decode(both, x.used, both.length - x.used);
        eq(y.reqId, 2, "frame2 id");

        // 超限拒绝
        try {
            Proto.encode(1, Proto.T_REQ, new byte[9 * 1024 * 1024]);
            throw new AssertionError("oversize must throw");
        } catch (IllegalArgumentException ok) { }

        // 畸形 len (超 MAX_PAYLOAD 头) → decode 拒绝
        byte[] evil = new byte[12];
        evil[11] = (byte) 0xFF; evil[10] = (byte) 0xFF; evil[9] = (byte) 0xFF; evil[8] = (byte) 0xFF;
        eq(Proto.decode(evil, 0, evil.length), null, "oversize header must be null");

        System.out.println("ProtoTest OK");
    }
}
