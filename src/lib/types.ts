import { AdminConfig } from '@/lib/admin.types';

export interface ApiSite {
  key: string;
  api: string;
  name: string;
  detail?: string;
  is_adult?: boolean;
  site_type?: number;
  spider?: string;
  searchable?: number;
}
// 播放记录数据结构
export interface PlayRecord {
  title: string;
  source_name: string;
  cover: string;
  year: string;
  index: number; // 第几集
  total_episodes: number; // 总集数
  play_time: number; // 播放进度（秒）
  total_time: number; // 总进度（秒）
  save_time: number; // 记录保存时间（时间戳）
  search_title: string; // 搜索时使用的标题
}

// 播放器配置类型
export interface PlayerConfig {
  block_ad_enabled: boolean;
  optimization_enabled: boolean;
  allow_lan_sources: boolean;
}

// 用户偏好配置类型（统一配置，包含原 SiteConfig 字段）
export interface UserPreferences {
  site_name: string;
  announcement: string;
  search_downstream_max_page: number;
  site_interface_cache_time: number;
  disable_yellow_filter: boolean;

  // 豆瓣设置
  douban_data_source: string;
  douban_proxy_url: string;
  douban_image_proxy_type: string;
  douban_image_proxy_url: string;

  // 用户偏好设置
  enable_optimization: boolean;
  fluid_search: boolean;
  player_buffer_mode: string;
  has_seen_announcement: string;
}

export interface RuntimeCustomCategory {
  name: string;
  type: 'movie' | 'tv';
  query: string;
}

export interface RuntimeConfigResponse {
  storage_type: string;
  use_local_source_config: boolean;
  site_name: string;
  announcement: string;
  douban_proxy_type: string;
  douban_proxy: string;
  douban_image_proxy_type: string;
  douban_image_proxy: string;
  disable_yellow_filter: boolean;
  fluid_search: boolean;
  custom_categories: RuntimeCustomCategory[];
}

// 播放器初始化状态类型
export interface PlayerInitialState {
  detail: SearchResult;
  other_sources: SearchResult[];
  play_record: {
    episode_index: number;
    play_time: number;
  } | null;
  initial_episode_index: number;
  resume_time: number | null;
  is_favorited: boolean;
  skip_config: {
    enable: boolean;
    intro_time: number;
    outro_time: number;
  } | null;
  block_ad_enabled: boolean;
  optimization_enabled: boolean;
}

export interface SourceHealthStats {
  source_key: string;
  total_tests: number;
  successful_tests: number;
  failed_tests: number;
  success_rate: number;
  avg_response_time_ms: number;
  last_success_time?: number | null;
  last_failure_time?: number | null;
  last_available_time?: number | null;
  consecutive_failures: number;
  auto_degraded: boolean;
  recent_results: {
    success: boolean;
    response_time_ms: number;
    error_reason?: string | null;
    timestamp: number;
  }[];
}

export interface ChangePlaySourceResponse {
  detail: SearchResult;
  target_episode_index: number;
  resume_time: number;
}

export interface InitializePlayerByQueryResponse {
  results: SearchResult[];
  test_results: Array<[string, SourceTestResult]>;
}

/** Spider 网盘集 playerContent 解析结果 */
export interface ResolveEpisodeResponse {
  url: string;
  header: Record<string, string>;
  source_site_type: number;
}

// ---------------------------------------------------------------------------
// MediaResource (V2 Phase 1, 对应 crates/core/src/media.rs, 仅类型定义暂未接线)
// ---------------------------------------------------------------------------

/** 资源类型: 与 Rust ResourceType 的 snake_case 序列化保持一致 */
export type ResourceType =
  | 'file'
  | 'http'
  | 'hls'
  | 'dash'
  | 'local_file'
  | 'unknown';

export interface SubtitleResource {
  url: string;
  language?: string;
  format?: string;
}

export interface MediaMetadata {
  title?: string;
  episode?: string;
  duration?: number;
}

/** 所有播放入口的统一资源描述 (docs/architecture/01-core-model.md) */
export interface MediaResource {
  id: string;
  url: string;
  resourceType: ResourceType;
  headers: Record<string, string>;
  cookies: Record<string, string>;
  userAgent?: string;
  referer?: string;
  /** 是否必须经本地 PlaybackGateway 播放 */
  proxyRequired: boolean;
  subtitles: SubtitleResource[];
  metadata: MediaMetadata;
}

// ---------------------------------------------------------------------------
// Playback (V2 Phase 4, 对应 crates/core/src/playback/, 仅类型定义暂未接线)
// ---------------------------------------------------------------------------

/** 播放器状态: 与 Rust PlaybackStatus 的 snake_case 序列化保持一致 */
export type PlaybackStatus =
  | 'idle'
  | 'loading'
  | 'playing'
  | 'paused'
  | 'stopped'
  | 'ended'
  | 'error';

/** Rust → UI 状态快照 (playback_state / playback_state 命令返回值) */
export interface PlaybackState {
  status: PlaybackStatus;
  time: number;
  duration: number;
  /** 当前资源标识, 不含敏感直链 */
  resourceId?: string | null;
  /** status=error 时的可读信息 */
  error?: string | null;
}

// 收藏数据结构
export interface Favorite {
  source_name: string;
  total_episodes: number; // 总集数
  title: string;
  year: string;
  cover: string;
  save_time: number; // 记录保存时间（时间戳）
  search_title: string; // 搜索时使用的标题
  origin?: 'vod' | 'live';
}

// 存储接口
export interface IStorage {
  // 播放记录相关
  getPlayRecord(userName: string, key: string): Promise<PlayRecord | null>;
  setPlayRecord(
    userName: string,
    key: string,
    record: PlayRecord,
  ): Promise<void>;
  getAllPlayRecords(userName: string): Promise<{ [key: string]: PlayRecord }>;
  deletePlayRecord(userName: string, key: string): Promise<void>;

  // 收藏相关
  getFavorite(userName: string, key: string): Promise<Favorite | null>;
  setFavorite(userName: string, key: string, favorite: Favorite): Promise<void>;
  getAllFavorites(userName: string): Promise<{ [key: string]: Favorite }>;
  deleteFavorite(userName: string, key: string): Promise<void>;

  // 用户相关
  registerUser(userName: string, password: string): Promise<void>;
  verifyUser(userName: string, password: string): Promise<boolean>;
  // 检查用户是否存在（无需密码）
  checkUserExist(userName: string): Promise<boolean>;
  // 修改用户密码
  changePassword(userName: string, newPassword: string): Promise<void>;
  // 删除用户（包括密码、搜索历史、播放记录、收藏夹）
  deleteUser(userName: string): Promise<void>;

  // 搜索历史相关
  getSearchHistory(userName: string): Promise<string[]>;
  addSearchHistory(userName: string, keyword: string): Promise<void>;
  deleteSearchHistory(userName: string, keyword?: string): Promise<void>;

  // 用户列表
  getAllUsers(): Promise<string[]>;

  // 管理员配置相关
  getAdminConfig(): Promise<AdminConfig | null>;
  setAdminConfig(config: AdminConfig): Promise<void>;

  // 跳过片头片尾配置相关
  getSkipConfig(
    userName: string,
    source: string,
    id: string,
  ): Promise<SkipConfig | null>;
  setSkipConfig(
    userName: string,
    source: string,
    id: string,
    config: SkipConfig,
  ): Promise<void>;
  deleteSkipConfig(userName: string, source: string, id: string): Promise<void>;
  getAllSkipConfigs(userName: string): Promise<{ [key: string]: SkipConfig }>;

  // 数据清理相关
  clearAllData(): Promise<void>;
}

// 单站点内的一组源头 (线路)
export interface PlayGroup {
  flag: string;
  episodes: string[];
  episodes_titles: string[];
  episodes_raw: string[];
}

// 搜索结果数据结构
export interface SearchResult {
  id: string;
  title: string;
  poster: string;
  episodes: string[];
  episodes_titles: string[];
  source: string;
  source_name: string;
  class?: string;
  year: string;
  desc?: string;
  type_name?: string;
  douban_id?: number;
  source_site_type?: number;
  /** Spider 网盘集原始 id (与 episodes 对齐) */
  episodes_raw?: string[];
  /** 该详情内部的多组源头 (线路); 顶层 episodes 为当前默认组 */
  play_groups?: PlayGroup[];
}

/** 聚合后的分组*/
export interface AggregatedGroup {
  representative: SearchResult;
  episodes: number;
  source_names: string[];
  douban_id?: number;
}

/** 搜索过滤器*/
export interface SearchFilter {
  source: string;
  title: string;
  year: string;
  year_order: 'none' | 'asc' | 'desc';
}

export interface SearchPageBootstrap {
  search_history: string[];
  fluid_search: boolean;
}

/** 跳过动作*/
export type SkipAction = 'None' | { SkipIntro: number } | 'SkipOutro';

export interface PlayerTickDecision {
  shouldSaveProgress: boolean;
  nextLastSaveAtMs: number;
  nextLastSkipCheckAtMs: number;
  skipAction: SkipAction | null;
  didPreload: boolean;
}

// 豆瓣数据结构
export interface DoubanItem {
  id: string;
  title: string;
  poster: string;
  rate: string;
  year: string;
}

export interface DoubanResult {
  code: number;
  message: string;
  list: DoubanItem[];
}

export interface DoubanPageResponse {
  list: DoubanItem[];
  has_more: boolean;
}

export interface DoubanDefaultsResponse {
  primarySelection: string;
  secondarySelection: string;
  multiLevelSelection: Record<string, string>;
  cacheEnabled: boolean;
  requireSecondary: boolean;
}

// 跳过片头片尾配置数据结构
export interface SkipConfig {
  enable: boolean; // 是否启用跳过片头片尾
  intro_time: number; // 跳过片头时间（秒）
  outro_time: number; // 跳过片尾时间（秒）
}

export interface ApplySkipConfigResponse {
  deleted: boolean;
}

export enum UpdateStatus {
  CHECKING = 'Checking',
  HAS_UPDATE = 'HasUpdate',
  NO_UPDATE = 'NoUpdate',
  FETCH_FAILED = 'FetchFailed',
}

// Rust 数据结构类型
export interface RustFavorite {
  key: string;
  title: string;
  source_name: string;
  year: string;
  cover: string;
  episode_index: number;
  total_episodes: number;
  save_time: number;
  search_title: string;
}

export interface ContinueWatchingCard {
  key: string;
  source: string;
  id: string;
  title: string;
  source_name: string;
  year: string;
  poster: string;
  progress: number;
  episodes: number;
  currentEpisode: number;
  query: string;
  type: string;
}

export interface BangumiItem {
  id: number;
  name: string;
  name_cn: string;
  rating?: {
    score?: number;
  };
  air_date?: string;
  images?: {
    large?: string;
    common?: string;
    medium?: string;
    small?: string;
    grid?: string;
  };
}

export interface HomePageData {
  hotMovies: DoubanItem[];
  hotTvShows: DoubanItem[];
  hotVarietyShows: DoubanItem[];
  todayBangumi: BangumiItem[];
}

export interface HomeBootstrapResponse {
  resolvedWeekday: string;
  homeData: HomePageData;
  hasSeenAnnouncement: string;
  shouldShowAnnouncement: boolean;
}

export interface FavoriteCard {
  id: string;
  source: string;
  title: string;
  year: string;
  poster: string;
  episodes: number;
  source_name: string;
  currentEpisode?: number;
  search_title: string;
}

export interface BangumiCalendarData {
  weekday: {
    en: string;
  };
  items?: BangumiItem[];
}
// Rust 返回类型定义
export interface SourceTestResult {
  quality: string;
  load_speed: string;
  ping_time: number;
  has_error: boolean;
}

export interface PreferBestSourceResponse {
  best_source: SearchResult;
  test_results: Array<[string, SourceTestResult]>;
}

/** 首页目录：一个订阅源的分类 + 每分类下的视频 */
export interface HomeCatalogResponse {
  source_key: string;
  source_name: string;
  site_type: number;
  categories: HomeCategoryRow[];
}

export interface HomeCategoryRow {
  type_id: string;
  type_name: string;
  list: HomeVideoCard[];
}

export interface HomeVideoCard {
  id: string;
  title: string;
  poster: string;
  year?: string | null;
  episodes: string[];
  class?: string | null;
  source: string;
  source_name: string;
}
