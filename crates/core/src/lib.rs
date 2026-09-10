pub mod admin_config;
pub mod adult;
pub mod bridge;
pub mod gateway;
pub mod media;
pub mod netdisk;
pub mod netdisk_proxy;
pub mod playback;
pub mod qrcodelogin;
pub mod resolver;
pub mod search_aggregation;
pub mod source_selection;
pub mod spider;
pub mod types;

pub use admin_config::default_admin_config_value;
pub use admin_config::merge_admin_config_with_defaults;
pub use admin_config::normalize_source_config;
pub use admin_config::parse_admin_config;
pub use adult::{filter_adult_sources, is_adult_source};
pub use gateway::{
    create_session, drop_session, session_count, session_url, wrap_resource, ResourceSession,
};
pub use media::{
    parse_cookie_header, MediaMetadata, MediaResource, ResourceType, SubtitleResource,
};
pub use playback::{filter_ads_from_m3_u8, SkipAction, SkipDetection};
pub use resolver::{
    DirectResolver, LocalFileResolver, NetdiskResolver, RawPlayResult, ResolveError,
    ResolveInput, ResolverManager, SpiderPlayFetcher, SpiderResolver,
};
pub use search_aggregation::{
    aggregate_search_results, apply_filter, compute_group_stats, sort_by_year, AggregatedGroup,
    SearchFilter, YearOrder,
};
pub use source_selection::{
    calculate_source_score, prefer_best_source, test_video_source, SourceTestResult,
};
pub use types::SearchResult;
