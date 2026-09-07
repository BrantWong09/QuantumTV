//! 百度 passport 扫码 (Task 3 实现)
use super::{PollOutcome, QrSession};

pub(super) async fn start() -> Result<QrSession, String> {
    Err("baidu 扫码待实现".to_string())
}

pub(super) async fn poll(_session: &QrSession) -> Result<PollOutcome, String> {
    Err("baidu 扫码待实现".to_string())
}
