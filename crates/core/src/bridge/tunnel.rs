//! 桥接隧道: APK 主动注册 + 帧多路复用
//!
//! 帧格式 [u32 BE id][u32 BE len][payload]; APK 首帧 id=0 = 注册 JSON;
//! 之后桌面→APK 帧 payload = 完整 HTTP 请求字节, APK→桌面帧 = HTTP 响应体字节,
//! 按 id 配对。VirtualBridge 在 127.0.0.1:{host_port} 接 spider 层请求转进隧道。

use serde::Deserialize;

#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub id: u32,
    pub payload: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RegisterInfo {
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub apk: String,
}

/// [u32 BE id][u32 BE len][payload]
pub(crate) fn encode_frame(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// 从缓冲前缀解一帧; 不完整返回 None; 成功时从 buf 消耗掉对应字节
pub(crate) fn parse_frame(buf: &mut Vec<u8>) -> Option<(Frame, usize)> {
    if buf.len() < 8 {
        return None;
    }
    let id = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    if buf.len() < 8 + len {
        return None;
    }
    let payload = buf[8..8 + len].to_vec();
    buf.drain(..8 + len);
    Some((Frame { id, payload }, 8 + len))
}

/// 注册帧 payload → (device, apk)
pub(crate) fn parse_register(payload: &[u8]) -> Option<(String, String)> {
    let reg: RegisterInfo = serde_json::from_slice(payload).ok()?;
    Some((reg.device, reg.apk))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let raw = encode_frame(7, b"{\"code\":200}");
        assert_eq!(&raw[..4], &[0, 0, 0, 7]); // id BE
        assert_eq!(&raw[4..8], &[0, 0, 0, 12]); // len BE = 12
        let mut buf = raw.clone();
        let (f, used) = parse_frame(&mut buf).unwrap();
        assert_eq!(used, raw.len());
        assert_eq!(f.id, 7);
        assert_eq!(f.payload, b"{\"code\":200}");
    }

    #[test]
    fn frame_partial_buffer_returns_none() {
        let raw = encode_frame(1, b"hello");
        let mut buf = raw[..raw.len() - 2].to_vec(); // 缺 2 字节
        assert!(parse_frame(&mut buf).is_none());
        buf.extend_from_slice(&raw[raw.len() - 2..]);
        let (f, used) = parse_frame(&mut buf).unwrap();
        assert_eq!(f.payload, b"hello");
        assert_eq!(used, raw.len());
    }

    #[test]
    fn parse_register_json() {
        let (device, apk) = parse_register(br#"{"device":"MuMu","apk":"1.1"}"#).unwrap();
        assert_eq!(device, "MuMu");
        assert_eq!(apk, "1.1");
        assert!(parse_register(b"garbage").is_none());
    }
}
