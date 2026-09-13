pub mod analytics;
pub mod bangumi;
pub mod bridge;
pub mod cloud_drive;
pub mod config;
pub mod content_analyzer;
pub mod data_fusion;
pub mod douban_client;
pub mod home;
// V2 Phase 6: mpv_embed.rs / mpv_player.rs 旧播放器路径已删,
// mpv 唯一入口在 playback.rs (PlaybackManager)
pub mod netdisk;
pub mod playback;
pub mod preload;
pub mod recommendation;
pub mod search;
pub mod settings;
pub mod skip;
pub mod source_intelligence;
pub mod version;
pub mod version_check;
pub mod video;
