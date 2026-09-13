//! 网盘 Provider 层 (方案 V1 / ADR 0005)
//!
//! 四层解耦 (§49): 认证 (本模块) ≠ 分享解析 ≠ 播放解析 (spider 经 bridge, D2)
//! ≠ 播放代理 (gateway.rs)。Bridge 只做 RPC, Gateway 只管传输, 均不碰网盘认证。
//!
//! 每个网盘独立 Provider (§4): baidu / quark / uc 各自拥有扫码、验证与
//! PlayResource 语义; 新增网盘 (阿里/115/...) 只需新增一个 Provider 实现。

pub mod baidu;
pub mod cas;
pub mod crypto;
pub mod manager;
pub mod provider;
pub mod quark;
pub mod store;
pub mod types;
pub mod uc;

pub use crypto::CredentialCrypto;
pub use manager::{BridgeCookieSink, CloudDriveManager};
pub use provider::{BridgeCloudPlayFetcher, CloudDriveProvider, CloudPlayFetcher};
pub use store::CredentialStore;
pub use types::{
    AuthState, AuthStatus, CheckItem, CloudDriveCapabilities, CloudDriveError, CloudDriveType,
    CloudFile, ConnectionTest, LoginResult, PlayResource, ProviderCredential, QrLoginSession,
    RawLoginOutcome, ShareResource, VerifyOutcome,
};
