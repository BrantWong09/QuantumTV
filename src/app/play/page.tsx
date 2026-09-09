/* eslint-disable @typescript-eslint/no-explicit-any, react-hooks/exhaustive-deps, no-console, @next/next/no-img-element */

'use client';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import Hls from 'hls.js';
import {
  FastForward,
  Heart,
  Pause,
  Play,
  Rewind,
  SkipBack,
  SkipForward,
  Volume2,
} from 'lucide-react';
import { useRouter, useSearchParams } from 'next/navigation';
import * as Plyr from 'plyr';
import { Suspense, useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';

import {
  ApplySkipConfigResponse,
  InitializePlayerByQueryResponse,
  PlayerInitialState,
  PlayerTickDecision,
  ResolveEpisodeResponse,
  SearchResult,
} from '@/lib/types';
import { appLayoutClasses } from '@/lib/ui-layout';
import { cn, generateStorageKey, subscribeToDataUpdates } from '@/lib/utils';
import { useProxyImage } from '@/hooks/useProxyImage';

import EpisodeSelector from '@/components/EpisodeSelector';
import PageLayout from '@/components/PageLayout';
import SkipConfigPanel from '@/components/SkipConfigPanel';
import Toast from '@/components/Toast';

// 扩展 HTMLVideoElement 类型以支持 hls 属性
declare global {
  interface HTMLVideoElement {
    hls?: any;
  }
}

// Wake Lock API 类型声明
interface WakeLockSentinel {
  released: boolean;
  release(): Promise<void>;
  addEventListener(type: 'release', listener: () => void): void;
  removeEventListener(type: 'release', listener: () => void): void;
}

// -----------------------------------------------------------------------------
// 音量增强(>100%)
// 浏览器把 HTMLMediaElement.volume 限制在 0..1,超出不放大;只有经 Web Audio
// (GainNode 增益 >1)才能真实放大。注意:createMediaElementSource 会把 <video>
// 的音频永久改走音频图,而跨域无 CORS 的直连源在音频图中会静音,因此增强
// 仅在走 MSE(blob src)的 m3u8 播放路径上启用。
// -----------------------------------------------------------------------------
const VOLUME_MAX = 3; // 上限 300%
const VOLUME_STEP = 0.1; // 每档 10%
const VOLUME_STORAGE_KEY = 'quantum:player:volume';
const DEFAULT_VOLUME = 0.7;

/* eslint-disable no-undef -- Web Audio 类型未收录进 ESLint browser 环境,由 tsc 校验 */
interface VolumeBoostGraph {
  ctx: AudioContext;
  source: MediaElementAudioSourceNode;
  gain: GainNode;
  video: HTMLVideoElement;
}
/* eslint-enable no-undef */

const loadPersistedVolume = (): number => {
  if (typeof window === 'undefined') return DEFAULT_VOLUME;
  try {
    const raw = window.localStorage.getItem(VOLUME_STORAGE_KEY);
    if (raw === null) return DEFAULT_VOLUME;
    const v = Number(raw);
    if (Number.isNaN(v)) return DEFAULT_VOLUME;
    return Math.min(Math.max(v, 0), VOLUME_MAX);
  } catch {
    return DEFAULT_VOLUME;
  }
};

// 当前 <video> 是否可走 Web Audio 增强(仅 MSE/blob src 安全)
const videoCanBoost = (video: HTMLVideoElement | null | undefined): boolean => {
  if (!video) return false;
  try {
    return video.src.startsWith('blob:');
  } catch {
    return false;
  }
};

// 该地址是否会走 hls.js → MSE(WebView2 中 m3u8 均走此路径)
const isMsePlayableUrl = (url: string): boolean =>
  /\.m3u8($|\?)/i.test(url) && Hls.isSupported();

// mpv 嵌入模式(方案 B): 视频画面由原生子窗口渲染, 底部预留一条 DOM 控制条
const MPV_CONTROL_BAR_H = 48;
const MPV_SPEEDS = [1, 1.25, 1.5, 2, 3, 0.75, 0.5];

function PlayPageClient() {
  const router = useRouter();
  const searchParams = useSearchParams();

  // -----------------------------------------------------------------------------
  // 状态变量（State）
  // -----------------------------------------------------------------------------
  const [loading, setLoading] = useState(true);
  const [loadingStage, setLoadingStage] = useState<
    'searching' | 'preferring' | 'fetching' | 'ready'
  >('searching');
  const [loadingMessage, setLoadingMessage] = useState('正在搜索播放源...');
  const [error, setError] = useState<string | null>(null);
  const [detail, setDetail] = useState<SearchResult | null>(null);

  // 收藏状态
  const [favorited, setFavorited] = useState(false);

  // 跳过片头片尾配置
  const [skipConfig, setSkipConfig] = useState<{
    enable: boolean;
    intro_time: number;
    outro_time: number;
  }>({
    enable: false,
    intro_time: 0,
    outro_time: 0,
  });
  const skipConfigRef = useRef(skipConfig);
  useEffect(() => {
    skipConfigRef.current = skipConfig;
  }, [
    skipConfig,
    skipConfig.enable,
    skipConfig.intro_time,
    skipConfig.outro_time,
  ]);

  // 跳过检查的时间间隔控制
  const lastSkipCheckRef = useRef(0);

  // 去广告开关（从 Rust 配置读取，默认 true）
  const [blockAdEnabled, setBlockAdEnabled] = useState<boolean>(true);
  const blockAdEnabledRef = useRef(blockAdEnabled);
  useEffect(() => {
    blockAdEnabledRef.current = blockAdEnabled;
  }, [blockAdEnabled]);

  // 视频基本信息
  const [videoTitle, setVideoTitle] = useState(searchParams.get('title') || '');
  const [videoYear, setVideoYear] = useState(searchParams.get('year') || '');
  const [videoCover, setVideoCover] = useState('');
  const [, setVideoDoubanId] = useState(0);

  // 使用 Tauri proxy_image 命令加载封面图片
  const { url: proxiedCoverUrl } = useProxyImage(videoCover);
  // 当前源和ID
  const [currentSource, setCurrentSource] = useState(
    searchParams.get('source') || '',
  );
  const [currentId, setCurrentId] = useState(searchParams.get('id') || '');

  // 搜索所需信息
  const [searchTitle] = useState(searchParams.get('stitle') || '');
  const [searchType] = useState(searchParams.get('stype') || '');

  // 是否需要优选
  const [needPrefer, setNeedPrefer] = useState(
    searchParams.get('prefer') === 'true',
  );
  const needPreferRef = useRef(needPrefer);
  useEffect(() => {
    needPreferRef.current = needPrefer;
  }, [needPrefer]);
  // 集数相关
  const [currentEpisodeIndex, setCurrentEpisodeIndex] = useState(0);

  const currentSourceRef = useRef(currentSource);
  const currentIdRef = useRef(currentId);
  const videoTitleRef = useRef(videoTitle);
  const videoYearRef = useRef(videoYear);
  const detailRef = useRef<SearchResult | null>(detail);
  const currentEpisodeIndexRef = useRef(currentEpisodeIndex);

  // 同步最新值到 refs
  useEffect(() => {
    currentSourceRef.current = currentSource;
    currentIdRef.current = currentId;
    detailRef.current = detail;
    currentEpisodeIndexRef.current = currentEpisodeIndex;
    videoTitleRef.current = videoTitle;
    videoYearRef.current = videoYear;
  }, [
    currentSource,
    currentId,
    detail,
    currentEpisodeIndex,
    videoTitle,
    videoYear,
  ]);

  // 视频播放地址
  const [videoUrl, setVideoUrl] = useState('');
  // 异步解析(网盘直链)的请求序号, 用于丢弃过期回包
  const resolveTokenRef = useRef(0);

  // 实时下载速度窗口 (字节), HLS 用 FRAG_LOADED 统计, mp4 用解码增量估算
  const netSpeedWindowRef = useRef<{ bytes: number; at: number }>({
    bytes: 0,
    at: Date.now(),
  });
  // 速度徽章: 仅在速度明显变化时短暂显示, 静置自动隐藏
  const [speedBadge, setSpeedBadge] = useState<string | null>(null);
  const speedHideTimerRef = useRef<NodeJS.Timeout | null>(null);
  const lastShownBpsRef = useRef(0);
  const showSpeedBadge = (bps: number) => {
    const text =
      bps >= 1024 * 1024
        ? `${(bps / 1024 / 1024).toFixed(2)} MB/s`
        : `${Math.max(1, Math.round(bps / 1024))} KB/s`;
    setSpeedBadge(text);
    if (speedHideTimerRef.current) {
      clearTimeout(speedHideTimerRef.current);
    }
    speedHideTimerRef.current = setTimeout(() => setSpeedBadge(null), 3000);
  };

  // 详情内部当前选中的源头组下标 (对齐 TVBox 线路切换; 仅多组时显示)
  const [activeGroupIndex, setActiveGroupIndex] = useState(0);

  // 换新片时的原始选集 (含后端首集直链化结果), 切回默认组时还原
  const defaultEpisodesRef = useRef<{
    episodes: string[];
    episodes_titles: string[];
    episodes_raw: string[];
  } | null>(null);

  // 总集数
  const totalEpisodes = detail?.episodes?.length || 0;

  // 换新片时, 重置源头组为后端默认组 (集数最多的一组, 与后端 parse 规则一致)
  useEffect(() => {
    if (!detail) return;
    defaultEpisodesRef.current = {
      episodes: detail.episodes,
      episodes_titles: detail.episodes_titles,
      episodes_raw: detail.episodes_raw || [],
    };
    if (!detail.play_groups?.length) {
      setActiveGroupIndex(0);
      return;
    }
    const maxCount = Math.max(
      ...detail.play_groups.map((g) => g.episodes.length),
    );
    const defaultIdx = detail.play_groups.findIndex(
      (g) => g.episodes.length === maxCount,
    );
    setActiveGroupIndex(defaultIdx >= 0 ? defaultIdx : 0);
  }, [detail?.source, detail?.id]);

  // 用于记录是否需要在播放器 ready 后跳转到指定进度
  const resumeTimeRef = useRef<number | null>(null);
  // 上次使用的音量(可 >1 表示增强),默认 0.7
  const lastVolumeRef = useRef<number>(loadPersistedVolume());
  // 音量增强 Web Audio 链路:<video> 一旦被接管就无法解绑,与播放器同生命周期
  const boostGraphRef = useRef<VolumeBoostGraph | null>(null);
  // 上次使用的播放速率，默认 1.0
  const lastPlaybackRateRef = useRef<number>(1.0);
  // 最近一次已成功加载并开始播放的视频地址（用于判断是否为切换集数/换源）
  const lastStartedUrlRef = useRef<string>('');

  // 优选开关（从 Rust 配置读取，默认 true，用于后端优选源）
  const [optimizationEnabled, setOptimizationEnabled] = useState<boolean>(true);

  // 折叠状态（仅在 lg 及以上屏幕有效）
  const [isEpisodeSelectorCollapsed, setIsEpisodeSelectorCollapsed] =
    useState(false);

  // 跳过片头片尾设置面板状态
  const [isSkipConfigPanelOpen, setIsSkipConfigPanelOpen] = useState(false);

  // 页面全屏（网页全屏）：播放器铺满应用窗口，但不进入系统全屏。
  // 用于替代画中画——WebView2 的 PiP 小窗带原生齿轮，点击会跳 edge:// 错误页。
  const [isPageFullscreen, setIsPageFullscreen] = useState(false);
  const isPageFullscreenRef = useRef(isPageFullscreen);
  useEffect(() => {
    isPageFullscreenRef.current = isPageFullscreen;
  }, [isPageFullscreen]);

  // 页面全屏时锁定滚动（与 UserMenu 一致：只改 overflow，避免布局跳动）
  useEffect(() => {
    if (!isPageFullscreen) return;
    if (typeof document === 'undefined') return;

    const body = document.body;
    const html = document.documentElement;
    const originalBodyOverflow = body.style.overflow;
    const originalHtmlOverflow = html.style.overflow;

    body.style.overflow = 'hidden';
    html.style.overflow = 'hidden';

    return () => {
      body.style.overflow = originalBodyOverflow;
      html.style.overflow = originalHtmlOverflow;
    };
  }, [isPageFullscreen]);

  // Toast 通知状态
  const [toast, setToast] = useState<{
    show: boolean;
    message: string;
    type: 'success' | 'error' | 'info';
  }>({
    show: false,
    message: '',
    type: 'info',
  });

  // 显示 Toast 通知
  const showToast = (
    message: string,
    type: 'success' | 'error' | 'info' = 'info',
  ) => {
    setToast({ show: true, message, type });
  };

  // 音量增强系数(>1 时在播放器上显示角标)
  const [boostLevel, setBoostLevel] = useState<number | null>(null);

  // 视频加载状态
  const [isVideoLoading, setIsVideoLoading] = useState(true);
  const [swipeSeekOverlay, setSwipeSeekOverlay] = useState<{
    direction: 'forward' | 'backward';
    seconds: number;
    targetTime: number;
  } | null>(null);
  const [swipeSeekOverlayPortalHost, setSwipeSeekOverlayPortalHost] =
    useState<HTMLElement | null>(null);
  // 全屏时的弹窗挂载点。全屏元素（原生 top-layer 或 Plyr fallback 的
  // z-index:10000000）会盖住普通 DOM 里的面板，必须把面板挂进全屏元素内部。
  // 与 swipeSeekOverlayPortalHost 分开维护：后者仅在触屏设备上生效。
  const [modalPortalHost, setModalPortalHost] = useState<HTMLElement | null>(
    null,
  );

  // 播放进度保存相关
  const saveIntervalRef = useRef<NodeJS.Timeout | null>(null);
  const lastSaveTimeRef = useRef<number>(0);
  const swipeSeekOverlayTimerRef = useRef<NodeJS.Timeout | null>(null);

  const plyrRef = useRef<Plyr | null>(null);
  const videoElementRef = useRef<HTMLVideoElement | null>(null);
  const playerContainerRef = useRef<HTMLDivElement | null>(null);
  const hlsRef = useRef<Hls | null>(null);
  const gestureTouchStartRef = useRef<{
    x: number;
    y: number;
    timestamp: number;
  } | null>(null);
  const gestureStartPlayerTimeRef = useRef<number | null>(null);
  const lastTapRef = useRef<{
    x: number;
    y: number;
    timestamp: number;
  } | null>(null);

  // Wake Lock 相关
  const wakeLockRef = useRef<WakeLockSentinel | null>(null);

  // -----------------------------------------------------------------------------
  // 工具函数（Utils）

  // 更新视频地址
  // 是否为可直接播放的 http(s) 视频/m3u8 地址 (非网盘 raw id)。
  // 注意: 本地网盘代理地址(/netdisk/file.mp4?url=..)扩展名伪装成 mp4, 命中 mp4 分支。
  const isDirectPlayableUrl = (url: string): boolean =>
    /^https?:\/\//i.test(url) && /\.(m3u8|m3u|mp4|flv|mpd)(\?.*)?$/i.test(url);

  // Spider 网盘集: raw id → playerContent → 真实直链; 失败时返回 null 并给出可读原因
  const resolveEpisodeUrl = async (
    detailData: SearchResult,
    episodeIndex: number,
  ): Promise<string | null> => {
    const rawId =
      detailData.episodes_raw?.[episodeIndex] ||
      detailData.episodes[episodeIndex];
    if (!rawId) return null;
    // flag 取当前活跃组; 无组信息时退化为空串
    const group = detailData.play_groups?.[activeGroupIndex];
    const flag = group?.flag ?? '';
    console.log(
      `[播放解析] 请求解析: source=${detailData.source} flag=${flag} episodeIndex=${episodeIndex} rawId=${rawId.slice(0, 80)}`,
    );
    try {
      const resp = await invoke<ResolveEpisodeResponse>(
        'resolve_spider_episode',
        { source: detailData.source, flag, episodeId: rawId },
      );
      if (resp && resp.url) {
        console.log(
          `[播放解析] 解析成功: url=${resp.url.slice(0, 120)}… header=${JSON.stringify(resp.header)}`,
        );
        return resp.url;
      }
      console.warn('[播放解析] 解析返回空 url');
      return null;
    } catch (err) {
      const msg =
        (err as { toString?: () => string })?.toString?.() || '解析视频源失败';
      console.error('[播放解析] 解析失败:', err);
      showToast(msg, 'error');
      return null;
    }
  };

  // 更新视频地址: spider 网盘源先解析 raw id → 直链, 再交给播放器
  const updateVideoUrl = async (
    detailData: SearchResult | null,
    episodeIndex: number,
  ) => {
    const token = ++resolveTokenRef.current;
    if (
      !detailData ||
      !detailData.episodes ||
      episodeIndex >= detailData.episodes.length
    ) {
      setVideoUrl('');
      return;
    }
    const candidate = detailData.episodes[episodeIndex] || '';
    const isSpider = detailData.source_site_type === 3;

    // 非 spider, 或已是可直接播放地址, 或首集已由后端直链化 → 直接用
    if (!isSpider || isDirectPlayableUrl(candidate)) {
      console.log(
        `[播放] 直接播放: episodeIndex=${episodeIndex} isSpider=${isSpider} url=${candidate.slice(0, 120)}`,
      );
      if (candidate !== videoUrl) {
        setVideoUrl(candidate);
      }
      return;
    }

    // spider raw id: 经 playerContent 解析为直链后播放
    setIsVideoLoading(true);
    const resolved = await resolveEpisodeUrl(detailData, episodeIndex);
    if (token !== resolveTokenRef.current) {
      return; // 集数/详情已切换, 丢弃过期结果
    }
    if (resolved) {
      // 换链成功(与失败链不同) → 重置重试计数, 新链仍可重试 1 次
      if (
        directRetryRef.current.url &&
        directRetryRef.current.url !== resolved
      ) {
        directRetryRef.current = { url: '', count: 0 };
      }
      setVideoUrl(resolved);
    } else {
      // 解析失败(网盘未登录/接口异常): 清空地址让播放器退出加载态
      setVideoUrl('');
      setIsVideoLoading(false);
    }
  };

  // mp4 直链加载失败后的重试状态: 网盘 dlink 签名有效期短(实测分钟级),
  // 播放器报错时重新解析当前集换新链, 最多 1 次防止死循环
  const directRetryRef = useRef<{ url: string; count: number }>({
    url: '',
    count: 0,
  });

  // mpv 嵌入播放(方案 B): mpv --wid 渲染进主窗口子窗口, 控制走 JSON IPC。
  // 换源/切集 = Rust 侧复用进程 + loadfile replace; 状态经 mpv-embed-event 回传。
  const [mpvState, setMpvState] = useState<{
    active: boolean;
    time: number;
    duration: number;
    paused: boolean;
    eof: boolean;
  }>({ active: false, time: 0, duration: 0, paused: false, eof: false });
  const mpvStateRef = useRef(mpvState);
  useEffect(() => {
    mpvStateRef.current = mpvState;
  }, [mpvState]);
  const mpvActiveRef = useRef(false);
  useEffect(() => {
    mpvActiveRef.current = mpvState.active;
  }, [mpvState.active]);
  // 已通过 loadfile 播放的地址 (同集不重复下发)
  const mpvLoadedUrlRef = useRef('');
  // 退出 mpv 模式后 +1, 驱动 Plyr 重新加载
  const [plyrReloadTick, setPlyrReloadTick] = useState(0);
  // 嵌入模式本地控制状态
  const [mpvSpeedIdx, setMpvSpeedIdx] = useState(0);
  const [mpvVolume, setMpvVolume] = useState(70);

  const currentEpisodeTitle = () => {
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    return d?.episodes_titles?.[idx] || `${d?.title || '视频'} 第${idx + 1}集`;
  };

  // 拉起/换源 mpv 嵌入播放。进程与管道由 Rust 复用管理。
  const launchMpvExternal = async (url: string, epTitle: string) => {
    try {
      const sameUrl = mpvLoadedUrlRef.current === url;
      const startTime = sameUrl
        ? null
        : videoElementRef.current?.currentTime || 0;
      // 彻底卸掉 webview 内的 Plyr/hls/<video>: 否则旧播放器的控制条
      // (双进度条)仍显示、音频继续播、直接源黑屏检测还会反复触发
      cleanupPlayer();
      lastStartedUrlRef.current = '';
      await invoke('mpv_embed_launch', { url });
      if (!sameUrl) {
        const startOpts =
          startTime && startTime > 3
            ? ['loadfile', url, 'replace', `start=${startTime}`]
            : ['loadfile', url, 'replace'];
        await invoke('mpv_embed_command', { cmd: startOpts });
        await invoke('mpv_embed_command', {
          cmd: ['set_property', 'force-media-title', epTitle],
        });
        mpvLoadedUrlRef.current = url;
      }
      // 暂停 webview 内的播放器, 避免双声
      videoElementRef.current?.pause();
      setIsVideoLoading(false);
      setMpvState((s) => {
        if (s.active) return s;
        showToast('已切换到 mpv 嵌入播放', 'info');
        return {
          active: true,
          time: startTime || 0,
          duration: 0,
          paused: false,
          eof: false,
        };
      });
    } catch (err) {
      console.error('[播放] mpv 嵌入拉起失败:', err);
      showToast(String(err), 'error');
    }
  };

  // 退出 mpv 嵌入模式; backToPlyr=true 时重载内置播放器
  const exitMpvMode = async (backToPlyr = true) => {
    if (mpvActiveRef.current) {
      setMpvState({
        active: false,
        time: 0,
        duration: 0,
        paused: false,
        eof: false,
      });
    }
    mpvLoadedUrlRef.current = '';
    try {
      await invoke('mpv_embed_close');
    } catch {
      /* mpv 可能已自行退出, 忽略 */
    }
    if (backToPlyr) {
      // 先卸掉 mpv 模式遗留 (无), 再重建 Plyr: reloadTick 驱动
      // 主加载 effect 重跑, 前端 UI 不依赖 mpv 进程状态
      setPlyrReloadTick((t) => t + 1);
    }
  };

  // 确保视频源
  const ensureVideoSource = (video: HTMLVideoElement | null, url: string) => {
    if (!video || !url) return;
    const sources = Array.from(video.getElementsByTagName('source'));
    const existed = sources.some((s) => s.src === url);
    if (!existed) {
      // 移除旧的 source，保持唯一
      sources.forEach((s) => s.remove());
      const sourceEl = document.createElement('source');
      sourceEl.src = url;
      video.appendChild(sourceEl);
    }

    // 始终允许远程播放（AirPlay / Cast）
    video.disableRemotePlayback = false;
    // 如果曾经有禁用属性，移除之
    if (video.hasAttribute('disableRemotePlayback')) {
      video.removeAttribute('disableRemotePlayback');
    }
  };

  // Wake Lock 相关函数
  const requestWakeLock = async () => {
    try {
      if ('wakeLock' in navigator) {
        wakeLockRef.current = await (navigator as any).wakeLock.request(
          'screen',
        );
        console.log('Wake Lock 已启用');
      }
    } catch (err) {
      console.warn('Wake Lock 请求失败:', err);
    }
  };

  const releaseWakeLock = async () => {
    try {
      if (wakeLockRef.current) {
        await wakeLockRef.current.release();
        wakeLockRef.current = null;
        console.log('Wake Lock 已释放');
      }
    } catch (err) {
      console.warn('Wake Lock 释放失败:', err);
    }
  };

  const persistVolume = (v: number) => {
    try {
      if (typeof window !== 'undefined') {
        window.localStorage.setItem(VOLUME_STORAGE_KEY, String(v));
      }
    } catch {
      /* localStorage 不可用时静默忽略 */
    }
  };

  // 销毁增强链路(仅当需要重建 <video> 时调用:<video> 被 Web Audio
  // 接管后无法解绑,只能销毁重建)
  const teardownBoostGraph = () => {
    const g = boostGraphRef.current;
    if (!g) return;
    boostGraphRef.current = null;
    try {
      void g.ctx.close();
    } catch {
      /* 忽略关闭异常 */
    }
  };

  const unlockBoostContext = () => {
    const g = boostGraphRef.current;
    if (g && g.ctx.state === 'suspended') {
      g.ctx.resume().catch(() => {});
    }
  };

  // 为当前 <video> 建立 Web Audio 增益链路(element → gain → destination)
  const ensureBoostGraph = (video: HTMLVideoElement): boolean => {
    const existing = boostGraphRef.current;
    if (existing) {
      if (existing.video === video) return true;
      teardownBoostGraph();
    }
    if (typeof window === 'undefined') return false;
    const Ctor =
      window.AudioContext ||
      // eslint-disable-next-line no-undef -- Web Audio 类型由 tsc 校验
      (window as unknown as { webkitAudioContext?: typeof AudioContext })
        .webkitAudioContext;
    if (!Ctor) return false;
    try {
      const ctx = new Ctor();
      const source = ctx.createMediaElementSource(video);
      const gain = ctx.createGain();
      gain.gain.value = 1;
      source.connect(gain);
      gain.connect(ctx.destination);
      boostGraphRef.current = { ctx, source, gain, video };
      return true;
    } catch (err) {
      console.warn('创建音量增强链路失败:', err);
      return false;
    }
  };

  // 应用"有效音量":0..1 只设元素音量;>1 时元素音量固定为 1、由 GainNode
  // 放大。返回 >1 部分是否真的生效(不可增强的直连源返回 false)。
  const applyVolume = (v: number): boolean => {
    const clamped = Math.min(Math.max(v, 0), VOLUME_MAX);
    const video = videoElementRef.current;
    if (!video) return clamped <= 1;
    const boosted = clamped > 1;

    if (!boosted) {
      if (plyrRef.current) {
        plyrRef.current.volume = clamped;
      } else {
        video.volume = clamped;
      }
      if (boostGraphRef.current) {
        boostGraphRef.current.gain.gain.value = 1;
      }
      setBoostLevel(null);
      return true;
    }

    if (!videoCanBoost(video)) {
      // 直连跨域源在音频图里会静音:不能增强,退回元素音量 1(100%)
      if (plyrRef.current) {
        plyrRef.current.volume = 1;
      } else {
        video.volume = 1;
      }
      setBoostLevel(null);
      return false;
    }

    if (!ensureBoostGraph(video)) {
      setBoostLevel(null);
      return false;
    }
    unlockBoostContext();
    const g = boostGraphRef.current;
    if (g) g.gain.gain.value = clamped;
    if (plyrRef.current) {
      plyrRef.current.volume = 1;
    } else {
      video.volume = 1;
    }
    setBoostLevel(Math.round(clamped * 100) / 100);
    return true;
  };

  // 增减音量(快捷键用),>100% 部分受源类型限制
  const changeVolume = (delta: number): void => {
    if (!plyrRef.current) return;
    const target = Math.round((lastVolumeRef.current + delta) * 10) / 10;
    if (target === lastVolumeRef.current) return;
    if (target < 0 || target > VOLUME_MAX) return;

    if (target <= 1) {
      applyVolume(target);
      lastVolumeRef.current = target;
      persistVolume(target);
      showToast(`音量: ${Math.round(target * 100)}%`, 'info');
      return;
    }

    if (applyVolume(target)) {
      lastVolumeRef.current = target;
      persistVolume(target);
      showToast(`音量: ${Math.round(target * 100)}%`, 'info');
      return;
    }

    // 目标 >100% 但当前源不可增强:
    // - 上升时若还停留在旧的增强值,压缩回 100%
    // - 下降时先落到 100%,方便继续减小
    if (lastVolumeRef.current > 1) {
      applyVolume(1);
      lastVolumeRef.current = 1;
      persistVolume(1);
      showToast('当前片源不支持超过 100% 的音量，已调整为 100%', 'info');
    } else if (delta > 0) {
      showToast('当前片源不支持超过 100% 的音量', 'info');
    }
  };

  // 清理播放器资源的统一函数
  const cleanupPlayer = () => {
    try {
      teardownBoostGraph();

      if (hlsRef.current) {
        hlsRef.current.destroy();
        hlsRef.current = null;
      }

      if (videoElementRef.current?.hls) {
        videoElementRef.current.hls.destroy();
        delete videoElementRef.current.hls;
      }

      if (plyrRef.current) {
        plyrRef.current.destroy();
        plyrRef.current = null;
      }

      if (playerContainerRef.current) {
        playerContainerRef.current.innerHTML = '';
      }

      videoElementRef.current = null;
      console.log('清理播放器资源');
    } catch (err) {
      console.warn('清理播放器资源失败:', err);
      plyrRef.current = null;
      hlsRef.current = null;
      videoElementRef.current = null;
    }
  };

  // 跳过片头片尾配置相关函数
  const handleSkipConfigChange = async (newConfig: {
    enable: boolean;
    intro_time: number;
    outro_time: number;
  }) => {
    if (!currentSourceRef.current || !currentIdRef.current) return;

    console.log('[跳过配置] 更新配置', {
      old: skipConfigRef.current,
      new: newConfig,
    });

    try {
      setSkipConfig(newConfig);
      // 立即更新 ref，确保 timeupdate 事件处理器使用最新值
      skipConfigRef.current = newConfig;

      console.log('[跳过配置] 更新 ref', skipConfigRef.current);

      const response = await invoke<ApplySkipConfigResponse>(
        'apply_skip_config',
        {
          request: {
            source: currentSourceRef.current,
            id: currentIdRef.current,
            enable: newConfig.enable,
            intro_time: newConfig.intro_time,
            outro_time: newConfig.outro_time,
          },
        },
      );

      if (response.deleted) {
        showToast('已清除跳过设置', 'info');
      } else {
        const introText =
          newConfig.intro_time > 0
            ? `片头: ${formatTime(newConfig.intro_time)}`
            : '';
        const outroText =
          newConfig.outro_time < 0
            ? `片尾: ${formatTime(Math.abs(newConfig.outro_time))}`
            : '';
        const separator = introText && outroText ? '\n' : '';
        const message = newConfig.enable
          ? `已设置跳过配置：${introText}${separator}${outroText}`
          : '已取消跳过配置';

        showToast(message, 'success');
      }
      console.log('[跳过配置] 更新配置', newConfig);
    } catch (err) {
      console.error('[跳过配置] 更新配置失败:', err);
      showToast('更新跳过配置失败', 'error');
    }
  };

  const formatTime = (seconds: number): string => {
    if (seconds === 0) return '00:00';

    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const remainingSeconds = Math.round(seconds % 60);

    if (hours === 0) {
      // 不到一小时，格式为 00:00
      return `${minutes.toString().padStart(2, '0')}:${remainingSeconds
        .toString()
        .padStart(2, '0')}`;
    } else {
      // 超过一小时，格式为 00:00:00
      return `${hours.toString().padStart(2, '0')}:${minutes
        .toString()
        .padStart(2, '0')}:${remainingSeconds.toString().padStart(2, '0')}`;
    }
  };

  const showSwipeSeekOverlay = (
    direction: 'forward' | 'backward',
    seconds: number,
    targetTime: number,
  ) => {
    if (swipeSeekOverlayTimerRef.current) {
      clearTimeout(swipeSeekOverlayTimerRef.current);
      swipeSeekOverlayTimerRef.current = null;
    }
    setSwipeSeekOverlay({
      direction,
      seconds: Math.max(1, Math.round(seconds)),
      targetTime: Math.max(0, targetTime),
    });
  };

  const hideSwipeSeekOverlay = (delayMs = 0) => {
    if (swipeSeekOverlayTimerRef.current) {
      clearTimeout(swipeSeekOverlayTimerRef.current);
      swipeSeekOverlayTimerRef.current = null;
    }
    if (delayMs <= 0) {
      setSwipeSeekOverlay(null);
      return;
    }
    swipeSeekOverlayTimerRef.current = setTimeout(() => {
      setSwipeSeekOverlay(null);
      swipeSeekOverlayTimerRef.current = null;
    }, delayMs);
  };

  const resolvePlayerFullscreenElement = (): HTMLElement | null => {
    if (typeof document === 'undefined') return null;

    const container = playerContainerRef.current;
    if (!container) return null;

    // eslint-disable-next-line no-undef
    const docWithWebkitFullscreen = document as Document & {
      webkitFullscreenElement?: Element | null;
    };
    const candidates = [
      document.fullscreenElement,
      docWithWebkitFullscreen.webkitFullscreenElement ?? null,
    ];

    for (const candidate of candidates) {
      if (candidate instanceof HTMLElement && container.contains(candidate)) {
        return candidate;
      }
    }

    // Plyr 的 fallback 全屏不走 Fullscreen API：它给 .plyr 加
    // .plyr--fullscreen-fallback（position:fixed; z-index:10000000）。
    // 此时 document.fullscreenElement 为 null，但覆盖层仍需挂到该节点内。
    return container.querySelector<HTMLElement>('.plyr--fullscreen-fallback');
  };

  // 使用 Tauri fetch_binary 的 HLS.js Loader（带缓存和预取）
  class TauriHlsJsLoader {
    context: any;
    config: any;
    callbacks: any;
    stats: any;
    enableAdBlock: boolean;

    constructor(config: any) {
      this.config = config;
      this.enableAdBlock = config.enableAdBlock || false;

      console.log('[TauriHlsJsLoader] 初始化', {
        enableAdBlock: this.enableAdBlock,
      });

      // 在构造函数中立即初始化 stats
      this.stats = {
        aborted: false,
        loaded: 0,
        retry: 0,
        total: 0,
        chunkCount: 0,
        bwEstimate: 0,
        loading: { start: 0, first: 0, end: 0 },
        parsing: { start: 0, end: 0 },
        buffering: { start: 0, first: 0, end: 0 },
      };
    }

    destroy() {
      this.callbacks = null;
      this.config = null;
      this.stats = null;
      this.context = null;
    }

    abort() {
      if (this.stats) {
        this.stats.aborted = true;
      }
    }

    load(context: any, config: any, callbacks: any) {
      this.context = context;
      this.callbacks = callbacks;

      // 确保 stats 存在（以防万一）
      if (this.stats) {
        this.stats.loading.start = performance.now();
        this.stats.loading.first = 0;
        this.stats.loading.end = 0;
      }

      const { url } = context;

      // 对于 M3U8 manifest 和 level，使用 Rust 端的 fetch_m3u8 命令（支持去广告）
      if (context.type === 'manifest' || context.type === 'level') {
        console.log('[TauriHlsJsLoader] 加载M3U8', {
          url,
          type: context.type,
          enableAdBlock: this.enableAdBlock,
        });

        invoke<string>('fetch_m3u8', {
          url,
          enableAdBlock: this.enableAdBlock,
          headersOpt: null,
        })
          .then((m3u8Content) => {
            // 先检查 this.stats 是否为 null (即 loader 是否已被销毁)
            if (!this.stats || this.stats.aborted) return;

            this.stats.loading.end = performance.now();
            this.stats.loading.first = this.stats.loading.start;

            // M3U8 内容已经在 Rust 端处理完成（包括去广告）
            const textBytes = new TextEncoder().encode(m3u8Content);
            this.stats.loaded = textBytes.byteLength;
            this.stats.total = textBytes.byteLength;

            const response = {
              url,
              data: m3u8Content,
            };

            callbacks.onSuccess(response, this.stats, context);
          })
          .catch((error) => {
            // 同样在错误处理中检查 this.stats 是否存在
            if (!this.stats || this.stats.aborted) return;

            callbacks.onError({ code: 0, text: error.toString() }, context);
          });
      } else {
        // 对于 TS 分片等二进制内容，继续使用 fetch_binary
        invoke<{ status: number; body: number[] }>('fetch_binary', {
          url,
          method: 'GET',
          headersOpt: null,
        })
          .then((result) => {
            // 先检查 this.stats 是否为 null (即 loader 是否已被销毁)
            if (!this.stats || this.stats.aborted) return;

            this.stats.loading.end = performance.now();
            this.stats.loading.first = this.stats.loading.start;

            const data = new Uint8Array(result.body);
            this.stats.loaded = data.byteLength;
            this.stats.total = data.byteLength;
            const duration = this.stats.loading.end - this.stats.loading.start;
            this.stats.bwEstimate =
              duration > 0
                ? (this.stats.loaded * 8) / 1000 / 1000 / (duration / 1000)
                : 0;

            const response = {
              url,
              data: data.buffer,
            };

            callbacks.onSuccess(response, this.stats, context);
          })
          .catch((error) => {
            // 同样在错误处理中检查 this.stats 是否存在
            if (!this.stats || this.stats.aborted) return;

            callbacks.onError({ code: 0, text: error.toString() }, context);
          });
      }
    }
  }
  // 当集数索引变化时自动更新视频地址
  useEffect(() => {
    void updateVideoUrl(detail, currentEpisodeIndex);
  }, [detail, currentEpisodeIndex]);

  // 进入页面时直接获取全部源信息
  useEffect(() => {
    const initAll = async () => {
      // 中止仍在进行的聚合搜索: 已进入播放页, 后台搜索不再向桥接发请求
      try {
        await invoke('abort_active_search');
      } catch {
        // 忽略: 命令不可用时搜索照常结束
      }

      if (!currentSource && !currentId && !videoTitle && !searchTitle) {
        setError('缺少必要参数');
        setLoading(false);
        return;
      }

      setLoading(true);

      // 如果指定了 source 和 id，使用聚合初始化命令
      if (currentSource && currentId && !needPreferRef.current) {
        setLoadingStage('fetching');
        setLoadingMessage('🎬 正在初始化播放器...');

        try {
          const initialState = await invoke<PlayerInitialState>(
            'initialize_player_view',
            {
              source: currentSource,
              id: currentId,
              title: videoTitle || searchTitle,
            },
          );

          const detailData = initialState.detail;
          // 设置视频详情
          setNeedPrefer(false);
          setCurrentSource(detailData.source);
          setCurrentId(detailData.id);
          setVideoYear(detailData.year);
          setVideoTitle(detailData.title || videoTitleRef.current);
          setVideoCover(detailData.poster);
          setVideoDoubanId(detailData.douban_id || 0);
          setDetail(detailData);
          // 恢复播放记录
          setCurrentEpisodeIndex(initialState.initial_episode_index);
          resumeTimeRef.current = initialState.resume_time ?? null;

          // 设置收藏状态
          setFavorited(initialState.is_favorited);

          // 设置跳过配置
          if (initialState.skip_config) {
            setSkipConfig({
              enable: initialState.skip_config.enable,
              intro_time: initialState.skip_config.intro_time,
              outro_time: initialState.skip_config.outro_time,
            });
          }

          // 设置播放器配置
          setBlockAdEnabled(initialState.block_ad_enabled);
          setOptimizationEnabled(initialState.optimization_enabled);

          // 输出缓存统计信息
          invoke<
            Record<string, { entry_count: number; weighted_size: number }>
          >('get_cache_stats')
            .then((stats) => {
              console.log(
                '📊 缓存统计 | 视频缓存:',
                stats.video.entry_count,
                '条 | 搜索缓存:',
                stats.search.entry_count,
                '条',
              );
            })
            .catch(console.error);

          // 更新 URL
          const newUrl = new URL(window.location.href);
          newUrl.searchParams.set('source', detailData.source);
          newUrl.searchParams.set('id', detailData.id);
          newUrl.searchParams.set('year', detailData.year);
          newUrl.searchParams.set('title', detailData.title);
          newUrl.searchParams.delete('prefer');
          window.history.replaceState({}, '', newUrl.toString());

          setLoadingStage('ready');
          setLoadingMessage('✨ 准备就绪，即将开始播放...');
          setTimeout(() => setLoading(false), 1000);

          return;
        } catch (err) {
          console.error('初始化播放器失败:', err);
          setError('初始化播放器失败');
          setLoading(false);
          return;
        }
      }

      // 处理无 source/id 的情况 - 进行搜索
      const searchQuery = searchTitle || videoTitle;
      try {
        setLoadingStage('searching');
        setLoadingMessage('🔍 正在搜索播放源...');

        const response = await invoke<InitializePlayerByQueryResponse>(
          'initialize_player_by_query',
          {
            request: {
              query: searchQuery,
              filterTitle: videoTitleRef.current,
              year: videoYearRef.current || null,
              searchType: searchType || null,
              preferBest: optimizationEnabled,
            },
          },
        );

        if (!response.results || response.results.length === 0) {
          setError('未找到匹配结果');
          setLoading(false);
          return;
        }

        const detailData = response.results[0];

        setNeedPrefer(false);
        setCurrentSource(detailData.source);
        setCurrentId(detailData.id);
        setVideoYear(detailData.year);
        setVideoTitle(detailData.title || videoTitleRef.current);
        setVideoCover(detailData.poster);
        setVideoDoubanId(detailData.douban_id || 0);
        setDetail(detailData);
        if (currentEpisodeIndex >= detailData.episodes.length) {
          setCurrentEpisodeIndex(0);
        }

        const newUrl = new URL(window.location.href);
        newUrl.searchParams.set('source', detailData.source);
        newUrl.searchParams.set('id', detailData.id);
        newUrl.searchParams.set('year', detailData.year);
        newUrl.searchParams.set('title', detailData.title);
        newUrl.searchParams.delete('prefer');
        window.history.replaceState({}, '', newUrl.toString());

        setLoadingStage('ready');
        setLoadingMessage('加载完成，正在准备播放...');
        setTimeout(() => setLoading(false), 1000);
      } catch (err) {
        console.error('加载失败:', err);
        setError('加载失败');
        setLoading(false);
      }
    };

    initAll();
  }, []);

  useEffect(() => {
    document.addEventListener('keydown', handleKeyboardShortcuts);
    return () => {
      document.removeEventListener('keydown', handleKeyboardShortcuts);
    };
  }, []);

  // 自动播放策略下 AudioContext 初始可能处于 suspended,
  // 在任意用户手势后尝试恢复,保证增强链路出声
  useEffect(() => {
    if (typeof document === 'undefined') return;
    const unlock = () => unlockBoostContext();
    document.addEventListener('pointerdown', unlock, true);
    document.addEventListener('keydown', unlock, true);
    return () => {
      document.removeEventListener('pointerdown', unlock, true);
      document.removeEventListener('keydown', unlock, true);
    };
  }, []);

  // ---------------------------------------------------------------------------
  // 全屏弹窗挂载点跟踪（所有设备，含桌面端）
  // ---------------------------------------------------------------------------
  useEffect(() => {
    if (loading) return;

    const container = playerContainerRef.current;
    if (!container || typeof document === 'undefined') return;

    const syncModalHost = () => {
      const next = resolvePlayerFullscreenElement();
      setModalPortalHost((prev) => (prev === next ? prev : next));
    };

    syncModalHost();

    // fallback 全屏只改 class、不触发 fullscreenchange，故需 MutationObserver 兜底
    const observer = new MutationObserver(syncModalHost);
    observer.observe(container, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ['class'],
    });

    document.addEventListener('fullscreenchange', syncModalHost);
    document.addEventListener('webkitfullscreenchange', syncModalHost);

    return () => {
      observer.disconnect();
      document.removeEventListener('fullscreenchange', syncModalHost);
      document.removeEventListener('webkitfullscreenchange', syncModalHost);
      setModalPortalHost(null);
    };
  }, [loading]);

  // ---------------------------------------------------------------------------
  // 移动端手势
  // 双击：播放/暂停
  // 左右滑动：快退/快进
  // ---------------------------------------------------------------------------
  useEffect(() => {
    if (loading) return;

    const container = playerContainerRef.current;
    if (!container || typeof window === 'undefined') return;

    const isTouchDevice =
      window.matchMedia?.('(pointer: coarse)')?.matches ||
      'ontouchstart' in window;
    if (!isTouchDevice) return;

    const tapMaxDistance = 20;
    const doubleTapMaxDistance = 40;
    const doubleTapIntervalMs = 320;
    const swipeMinDistance = 45;
    const swipePreviewMinDistance = 18;
    const swipeHorizontalRatio = 1.3;
    const seekSecondsPerPx = 0.12;
    const maxSeekSeconds = 120;
    const seekStepSeconds = 5;

    const calculateSeekSeconds = (distancePx: number) => {
      const rawSeekSeconds = Math.min(
        maxSeekSeconds,
        Math.max(seekStepSeconds, distancePx * seekSecondsPerPx),
      );
      return Math.round(rawSeekSeconds / seekStepSeconds) * seekStepSeconds;
    };

    const seekPlayerBy = (seconds: number) => {
      const player = plyrRef.current;
      if (!player) return 0;

      const duration = Number(player.duration) || 0;
      if (duration <= 0) return 0;

      const currentTime = Number(player.currentTime) || 0;
      const targetTime = Math.max(
        0,
        Math.min(duration - 0.1, currentTime + seconds),
      );

      const actualSeek = targetTime - currentTime;
      if (Math.abs(actualSeek) < 0.5) return 0;

      player.currentTime = targetTime;
      showToast(
        actualSeek > 0
          ? `快进 ${Math.round(Math.abs(actualSeek))} 秒`
          : `后退 ${Math.round(Math.abs(actualSeek))} 秒`,
        'info',
      );
      return actualSeek;
    };

    const handleTouchStart = (event: TouchEvent) => {
      if (event.touches.length !== 1) {
        gestureTouchStartRef.current = null;
        gestureStartPlayerTimeRef.current = null;
        hideSwipeSeekOverlay();
        return;
      }

      const touch = event.touches[0];
      gestureTouchStartRef.current = {
        x: touch.clientX,
        y: touch.clientY,
        timestamp: Date.now(),
      };
      gestureStartPlayerTimeRef.current =
        Number(plyrRef.current?.currentTime) || 0;
      hideSwipeSeekOverlay();
    };

    const handleTouchMove = (event: TouchEvent) => {
      const start = gestureTouchStartRef.current;
      if (!start || event.touches.length !== 1) return;

      const touch = event.touches[0];
      const dx = touch.clientX - start.x;
      const dy = touch.clientY - start.y;
      const absDx = Math.abs(dx);
      const absDy = Math.abs(dy);

      const isHorizontalIntent =
        absDx >= swipePreviewMinDistance && absDx >= absDy * 1.05;

      if (!isHorizontalIntent) {
        if (absDy > absDx) {
          hideSwipeSeekOverlay();
        }
        return;
      }

      const player = plyrRef.current;
      const duration = Number(player?.duration) || 0;
      const baseTime = gestureStartPlayerTimeRef.current;
      if (!player || duration <= 0 || baseTime === null) return;

      if (event.cancelable) {
        event.preventDefault();
      }

      const signedSeconds = (dx > 0 ? 1 : -1) * calculateSeekSeconds(absDx);
      const targetTime = Math.max(
        0,
        Math.min(duration - 0.1, baseTime + signedSeconds),
      );
      const previewSeconds = Math.abs(targetTime - baseTime);
      if (previewSeconds < 1) return;

      showSwipeSeekOverlay(
        dx > 0 ? 'forward' : 'backward',
        previewSeconds,
        targetTime,
      );
    };

    const handleTouchEnd = (event: TouchEvent) => {
      const start = gestureTouchStartRef.current;
      gestureTouchStartRef.current = null;
      if (!start || event.changedTouches.length !== 1) return;

      const touch = event.changedTouches[0];
      const now = Date.now();
      const dx = touch.clientX - start.x;
      const dy = touch.clientY - start.y;
      const absDx = Math.abs(dx);
      const absDy = Math.abs(dy);
      const elapsed = now - start.timestamp;

      const isHorizontalSwipe =
        absDx >= swipeMinDistance && absDx >= absDy * swipeHorizontalRatio;
      if (isHorizontalSwipe) {
        const roundedSeekSeconds = calculateSeekSeconds(absDx);
        const baseTime = gestureStartPlayerTimeRef.current;
        const player = plyrRef.current;
        const duration = Number(player?.duration) || 0;
        if (player && duration > 0 && baseTime !== null) {
          const signedSeconds = (dx > 0 ? 1 : -1) * roundedSeekSeconds;
          const targetTime = Math.max(
            0,
            Math.min(duration - 0.1, baseTime + signedSeconds),
          );
          const actualSeek = targetTime - (Number(player.currentTime) || 0);
          if (Math.abs(actualSeek) >= 0.5) {
            player.currentTime = targetTime;
            showToast(
              actualSeek > 0
                ? `快进 ${Math.round(Math.abs(actualSeek))} 秒`
                : `后退 ${Math.round(Math.abs(actualSeek))} 秒`,
              'info',
            );
          }
          showSwipeSeekOverlay(
            dx > 0 ? 'forward' : 'backward',
            Math.abs(targetTime - baseTime),
            targetTime,
          );
          hideSwipeSeekOverlay(520);
        } else {
          const actualSeek = seekPlayerBy(
            dx > 0 ? roundedSeekSeconds : -roundedSeekSeconds,
          );
          if (Math.abs(actualSeek) >= 0.5) {
            const currentTime = Number(plyrRef.current?.currentTime) || 0;
            showSwipeSeekOverlay(
              dx > 0 ? 'forward' : 'backward',
              Math.abs(actualSeek),
              currentTime,
            );
            hideSwipeSeekOverlay(520);
          } else {
            hideSwipeSeekOverlay();
          }
        }
        gestureStartPlayerTimeRef.current = null;
        lastTapRef.current = null;
        return;
      }

      const isTap =
        absDx <= tapMaxDistance && absDy <= tapMaxDistance && elapsed <= 280;
      if (!isTap) {
        gestureStartPlayerTimeRef.current = null;
        hideSwipeSeekOverlay();
        return;
      }

      const lastTap = lastTapRef.current;
      if (lastTap && now - lastTap.timestamp <= doubleTapIntervalMs) {
        const tapDistance = Math.hypot(
          touch.clientX - lastTap.x,
          touch.clientY - lastTap.y,
        );

        if (tapDistance <= doubleTapMaxDistance && plyrRef.current) {
          // 双击 = 全屏切换(单击暂停交给 Plyr clickToPlay, 避免双重 toggle)
          if (event.cancelable) {
            event.preventDefault();
          }
          event.stopPropagation();
          event.stopImmediatePropagation();
          try {
            plyrRef.current.fullscreen.toggle();
          } catch {
            /* 全屏不可用时忽略 */
          }
          lastTapRef.current = null;
          gestureStartPlayerTimeRef.current = null;
          hideSwipeSeekOverlay();
          return;
        }
      }

      lastTapRef.current = {
        x: touch.clientX,
        y: touch.clientY,
        timestamp: now,
      };
      gestureStartPlayerTimeRef.current = null;
      hideSwipeSeekOverlay();
    };

    const handleTouchCancel = () => {
      gestureTouchStartRef.current = null;
      gestureStartPlayerTimeRef.current = null;
      hideSwipeSeekOverlay();
    };

    const syncSwipeSeekOverlayHost = () => {
      const fullscreenElement = resolvePlayerFullscreenElement();
      setSwipeSeekOverlayPortalHost((prev) => {
        if (prev === fullscreenElement) return prev;
        return fullscreenElement;
      });
    };

    let activeGestureTarget: HTMLElement | null = null;
    const addGestureListeners = (target: HTMLElement) => {
      target.addEventListener('touchstart', handleTouchStart, {
        passive: true,
      });
      target.addEventListener('touchmove', handleTouchMove, {
        passive: false,
      });
      target.addEventListener('touchend', handleTouchEnd, { passive: false });
      target.addEventListener('touchcancel', handleTouchCancel, {
        passive: true,
      });
    };

    const removeGestureListeners = (target: HTMLElement) => {
      target.removeEventListener('touchstart', handleTouchStart);
      target.removeEventListener('touchmove', handleTouchMove);
      target.removeEventListener('touchend', handleTouchEnd);
      target.removeEventListener('touchcancel', handleTouchCancel);
    };

    const resolveGestureTarget = () => {
      const fullscreenElement = resolvePlayerFullscreenElement();
      if (fullscreenElement) return fullscreenElement;

      const plyrRoot = container.querySelector<HTMLElement>('.plyr');
      return plyrRoot || container;
    };

    const syncGestureTarget = () => {
      const nextTarget = resolveGestureTarget();
      if (nextTarget === activeGestureTarget) return;

      if (activeGestureTarget) {
        removeGestureListeners(activeGestureTarget);
      }

      activeGestureTarget = nextTarget;
      if (activeGestureTarget) {
        addGestureListeners(activeGestureTarget);
      }
    };

    syncSwipeSeekOverlayHost();
    syncGestureTarget();

    const mutationObserver = new MutationObserver(() => {
      syncGestureTarget();
      syncSwipeSeekOverlayHost();
    });
    mutationObserver.observe(container, {
      childList: true,
      subtree: true,
    });

    const handleFullscreenChange = () => {
      syncGestureTarget();
      syncSwipeSeekOverlayHost();
    };
    document.addEventListener('fullscreenchange', handleFullscreenChange);

    return () => {
      mutationObserver.disconnect();
      document.removeEventListener('fullscreenchange', handleFullscreenChange);
      if (activeGestureTarget) {
        removeGestureListeners(activeGestureTarget);
      }
      setSwipeSeekOverlayPortalHost(null);
      gestureStartPlayerTimeRef.current = null;
      hideSwipeSeekOverlay();
    };
  }, [loading]);

  // ---------------------------------------------------------------------------
  // 集数切换
  // ---------------------------------------------------------------------------
  // 处理集数切换
  const handleEpisodeChange = (episodeNumber: number) => {
    if (episodeNumber >= 0 && episodeNumber < totalEpisodes) {
      // 在更换集数前保存当前播放进度
      if (plyrRef.current && plyrRef.current.paused) {
        saveCurrentPlayProgress();
      }
      setCurrentEpisodeIndex(episodeNumber);
    }
  };

  // 切换详情内部的源头组 (线路): 替换为选中组的选集并重播
  const handlePlayGroupChange = (index: number) => {
    const current = detailRef.current;
    const group = current?.play_groups?.[index];
    if (!current || !group || index === activeGroupIndex) return;
    if (group.episodes.length === 0) return;

    // 切换线路前保存当前播放进度
    if (plyrRef.current && !plyrRef.current.paused) {
      saveCurrentPlayProgress();
    }

    // 计算默认组 (集数最多的一组), 切回时还原原始选集 (含首集直链化)
    const maxCount = Math.max(
      ...current.play_groups!.map((g) => g.episodes.length),
    );
    const defaultIdx = current.play_groups!.findIndex(
      (g) => g.episodes.length === maxCount,
    );

    setActiveGroupIndex(index);
    const pristine = defaultEpisodesRef.current;
    // 保持当前集号, 越界则回第 1 集
    const targetLen =
      index === defaultIdx && pristine
        ? pristine.episodes.length
        : group.episodes.length;
    const nextIndex = currentEpisodeIndex < targetLen ? currentEpisodeIndex : 0;
    setCurrentEpisodeIndex(nextIndex);
    // 用选中组的选集重建 detail, 触发 updateVideoUrl + 播放器重载
    setDetail({
      ...current,
      episodes:
        index === defaultIdx && pristine ? pristine.episodes : group.episodes,
      episodes_titles:
        index === defaultIdx && pristine
          ? pristine.episodes_titles
          : group.episodes_titles,
      episodes_raw:
        index === defaultIdx && pristine
          ? pristine.episodes_raw
          : group.episodes_raw,
    });
  };

  const handlePreviousEpisode = () => {
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    if (d && d.episodes && idx > 0) {
      if (plyrRef.current && !plyrRef.current.paused) {
        saveCurrentPlayProgress();
      }
      setCurrentEpisodeIndex(idx - 1);
    }
  };

  const handleNextEpisode = () => {
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    if (d && d.episodes && idx < d.episodes.length - 1) {
      if (plyrRef.current && !plyrRef.current.paused) {
        saveCurrentPlayProgress();
      }
      setCurrentEpisodeIndex(idx + 1);
    }
  };

  const handleToggleBlockAd = async () => {
    const prevVal = blockAdEnabledRef.current;
    const newVal = !blockAdEnabledRef.current;
    // 乐观更新，保证 UI 立即反馈用户选择
    setBlockAdEnabled(newVal);
    blockAdEnabledRef.current = newVal;
    try {
      await invoke<void>('update_player_config', {
        config: { block_ad_enabled: newVal },
      });
      if (plyrRef.current) {
        resumeTimeRef.current = plyrRef.current.currentTime;
      }
      showToast(newVal ? '去广告已开启' : '去广告已关闭', 'success');
    } catch (err) {
      setBlockAdEnabled(prevVal);
      blockAdEnabledRef.current = prevVal;
      console.error('更新去广告配置失败', err);
      showToast('更新去广告配置失败', 'error');
    }
  };

  const handleToggleSkipEnable = () => {
    handleSkipConfigChange({
      ...skipConfigRef.current,
      enable: !skipConfigRef.current.enable,
    });
  };

  const handleSetIntroPoint = () => {
    const currentTime = plyrRef.current?.currentTime || 0;
    if (currentTime <= 0) return;
    handleSkipConfigChange({
      ...skipConfigRef.current,
      intro_time: currentTime,
    });
  };

  const handleSetOutroPoint = () => {
    const duration = plyrRef.current?.duration || 0;
    const currentTime = plyrRef.current?.currentTime || 0;
    const outroTime = -(duration - currentTime);
    if (outroTime >= 0) return;
    handleSkipConfigChange({
      ...skipConfigRef.current,
      outro_time: outroTime,
    });
  };

  const handleClearSkipConfig = () => {
    handleSkipConfigChange({
      enable: false,
      intro_time: 0,
      outro_time: 0,
    });
  };

  // 页面全屏切换。与系统全屏互斥：进入页面全屏前先退出系统全屏，
  // 否则两层"全屏"叠加会导致控制栏错位。
  const togglePageFullscreen = () => {
    setIsPageFullscreen((prev) => {
      const next = !prev;
      if (next && plyrRef.current?.fullscreen?.active) {
        try {
          plyrRef.current.fullscreen.exit();
        } catch (err) {
          console.warn('退出系统全屏失败:', err);
        }
      }
      return next;
    });
  };

  const enhancePlyrUi = () => {
    const container = playerContainerRef.current;
    if (!container) return;

    const controlsEl = container.querySelector<HTMLElement>('.plyr__controls');
    if (!controlsEl) return;

    const markControlsItem = (selector: string, className: string) => {
      const node = controlsEl.querySelector<HTMLElement>(selector);
      const item = node?.closest<HTMLElement>('.plyr__controls__item');
      if (item) {
        item.classList.add(className);
      }
    };

    let prevBtn = controlsEl.querySelector<HTMLButtonElement>(
      '.plyr__control--prev-episode',
    );
    if (!prevBtn) {
      prevBtn = document.createElement('button');
      prevBtn.type = 'button';
      prevBtn.className =
        'plyr__controls__item plyr__control plyr__control--prev-episode';
      prevBtn.setAttribute('aria-label', '播放上一集');
      prevBtn.innerHTML =
        '<svg width="18" height="18" viewBox="0 0 22 22" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M16 18L7.5 12L16 6V18ZM6 6V18H4V6H6Z" fill="currentColor"/></svg>';

      const playBtn = controlsEl.querySelector<HTMLButtonElement>(
        '.plyr__control[data-plyr="play"]',
      );
      if (playBtn?.parentElement) {
        playBtn.parentElement.insertBefore(prevBtn, playBtn);
      } else {
        controlsEl.prepend(prevBtn);
      }
    }

    let nextBtn = controlsEl.querySelector<HTMLButtonElement>(
      '.plyr__control--next-episode',
    );
    if (!nextBtn) {
      nextBtn = document.createElement('button');
      nextBtn.type = 'button';
      nextBtn.className =
        'plyr__controls__item plyr__control plyr__control--next-episode';
      nextBtn.setAttribute('aria-label', '播放下一集');
      nextBtn.innerHTML =
        '<svg width="18" height="18" viewBox="0 0 22 22" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M6 18l8.5-6L6 6v12zM16 6v12h2V6h-2z" fill="currentColor"/></svg>';

      const playBtn = controlsEl.querySelector<HTMLButtonElement>(
        '.plyr__control[data-plyr="play"]',
      );
      if (playBtn?.parentElement) {
        playBtn.parentElement.insertBefore(nextBtn, playBtn.nextSibling);
      } else {
        controlsEl.prepend(nextBtn);
      }
    }

    prevBtn.onclick = () => {
      handlePreviousEpisode();
    };
    const hasPrev = currentEpisodeIndexRef.current > 0;
    prevBtn.disabled = !hasPrev;
    prevBtn.title = hasPrev ? '播放上一集' : '已是第一集';

    nextBtn.onclick = () => {
      handleNextEpisode();
    };
    const hasNext =
      !!detailRef.current?.episodes &&
      currentEpisodeIndexRef.current <
        (detailRef.current?.episodes?.length || 1) - 1;
    nextBtn.disabled = !hasNext;
    nextBtn.title = hasNext ? '播放下一集' : '已是最后一集';

    // 页面全屏按钮：插在原生全屏按钮之前（原画中画按钮的位置）
    let pageFsBtn = controlsEl.querySelector<HTMLButtonElement>(
      '.plyr__control--page-fullscreen',
    );
    if (!pageFsBtn) {
      pageFsBtn = document.createElement('button');
      pageFsBtn.type = 'button';
      pageFsBtn.className =
        'plyr__controls__item plyr__control plyr__control--page-fullscreen';

      const nativeFsBtn = controlsEl.querySelector<HTMLButtonElement>(
        '.plyr__control[data-plyr="fullscreen"]',
      );
      if (nativeFsBtn?.parentElement) {
        nativeFsBtn.parentElement.insertBefore(pageFsBtn, nativeFsBtn);
      } else {
        controlsEl.appendChild(pageFsBtn);
      }
    }

    const pageFsActive = isPageFullscreenRef.current;
    // 未激活：四角向外的展开图标；激活：四角向内的收起图标
    pageFsBtn.innerHTML = pageFsActive
      ? '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>'
      : '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>';
    pageFsBtn.title = pageFsActive ? '退出页面全屏' : '页面全屏';
    pageFsBtn.setAttribute('aria-label', pageFsBtn.title);
    pageFsBtn.setAttribute('aria-pressed', pageFsActive ? 'true' : 'false');
    pageFsBtn.onclick = () => {
      togglePageFullscreen();
    };

    const settingsBtn = controlsEl.querySelector<HTMLButtonElement>(
      '.plyr__control[data-plyr="settings"]',
    );
    if (!settingsBtn) return;

    markControlsItem(
      '.plyr__control[data-plyr="play"]',
      'quantum-plyr-item-play',
    );
    markControlsItem('.plyr__control--prev-episode', 'quantum-plyr-item-prev');
    markControlsItem('.plyr__control--next-episode', 'quantum-plyr-item-next');
    markControlsItem('.plyr__time--current', 'quantum-plyr-item-time-current');
    markControlsItem(
      '.plyr__time--duration',
      'quantum-plyr-item-time-duration',
    );
    markControlsItem(
      '.plyr__control[data-plyr="mute"]',
      'quantum-plyr-item-mute',
    );
    markControlsItem('.plyr__volume', 'quantum-plyr-item-volume');
    markControlsItem(
      '.plyr__control[data-plyr="settings"]',
      'quantum-plyr-item-settings',
    );
    markControlsItem(
      '.plyr__control[data-plyr="pip"]',
      'quantum-plyr-item-pip',
    );
    markControlsItem(
      '.plyr__control--page-fullscreen',
      'quantum-plyr-item-page-fullscreen',
    );
    markControlsItem(
      '.plyr__control[data-plyr="airplay"]',
      'quantum-plyr-item-airplay',
    );
    markControlsItem(
      '.plyr__control[data-plyr="fullscreen"]',
      'quantum-plyr-item-fullscreen',
    );

    if (!settingsBtn.dataset.quantumHooked) {
      settingsBtn.dataset.quantumHooked = 'true';
      settingsBtn.addEventListener('click', () => {
        setTimeout(() => {
          enhancePlyrUi();
        }, 0);
      });
    }

    const menuId = settingsBtn.getAttribute('aria-controls');
    if (!menuId) return;

    const menuPanel = document.getElementById(menuId);
    const menuRoot =
      menuPanel?.querySelector<HTMLElement>('[id$="-home"] [role="menu"]') ||
      menuPanel?.querySelector<HTMLElement>('[role="menu"]');
    if (!menuRoot) return;

    let customGroup = menuRoot.querySelector<HTMLElement>(
      '[data-quantum-plyr-settings]',
    );
    if (!customGroup) {
      customGroup = document.createElement('div');
      customGroup.setAttribute('data-quantum-plyr-settings', 'true');
      customGroup.className = 'quantum-plyr-settings';
      menuRoot.appendChild(customGroup);
    }
    customGroup.innerHTML = '';

    const closeNativeSettings = () => {
      if (settingsBtn.getAttribute('aria-expanded') === 'true') {
        settingsBtn.click();
      }
    };

    const appendItem = (
      label: string,
      onClick: () => void,
      options?: { active?: boolean; danger?: boolean },
    ) => {
      const item = document.createElement('button');
      item.type = 'button';
      item.className = `plyr__control quantum-plyr-setting-item${
        options?.active ? ' is-active' : ''
      }${options?.danger ? ' is-danger' : ''}`;
      item.textContent = label;
      item.onclick = (event) => {
        event.preventDefault();
        event.stopPropagation();
        onClick();
        closeNativeSettings();
      };
      customGroup!.appendChild(item);
    };

    appendItem(
      `去广告${blockAdEnabledRef.current ? '(已开启)' : '(已关闭)'}`,
      () => {
        void handleToggleBlockAd();
      },
      { active: blockAdEnabledRef.current },
    );

    appendItem(
      `跳过片头片尾${skipConfigRef.current.enable ? '(已开启)' : '(已关闭)'}`,
      () => {
        handleToggleSkipEnable();
      },
      { active: skipConfigRef.current.enable },
    );

    appendItem(
      `设置片头 ${formatTime(skipConfigRef.current.intro_time)}`,
      () => {
        handleSetIntroPoint();
      },
      { active: skipConfigRef.current.intro_time > 0 },
    );

    appendItem(
      `设置片尾 ${
        skipConfigRef.current.outro_time < 0
          ? `-${formatTime(Math.abs(skipConfigRef.current.outro_time))}`
          : '--:--'
      }`,
      () => {
        handleSetOutroPoint();
      },
      { active: skipConfigRef.current.outro_time < 0 },
    );

    appendItem(
      '删除跳过配置',
      () => {
        handleClearSkipConfig();
      },
      { danger: true },
    );

    appendItem('打开跳过设置', () => {
      setIsSkipConfigPanelOpen(true);
    });
  };

  useEffect(() => {
    enhancePlyrUi();
  }, [
    blockAdEnabled,
    skipConfig.enable,
    skipConfig.intro_time,
    skipConfig.outro_time,
    currentEpisodeIndex,
    detail,
    isPageFullscreen,
  ]);

  // ---------------------------------------------------------------------------
  // 键盘快捷键
  // ---------------------------------------------------------------------------
  // 处理全局快捷键
  const handleKeyboardShortcuts = (e: KeyboardEvent) => {
    // 忽略输入框中的按键事件
    if (
      (e.target as HTMLElement).tagName === 'INPUT' ||
      (e.target as HTMLElement).tagName === 'TEXTAREA'
    )
      return;

    // ESC = 退出页面全屏（系统全屏由浏览器自己处理 ESC）
    if (e.key === 'Escape' && isPageFullscreenRef.current) {
      setIsPageFullscreen(false);
      e.preventDefault();
      return;
    }

    // mpv 嵌入模式: 播放控制经 IPC 下发
    if (mpvActiveRef.current) {
      const mpvKey = (cmd: (string | number)[]) => {
        void invoke('mpv_embed_command', { cmd }).catch(() => {
          /* IPC 未就绪时忽略 */
        });
      };
      if (e.key === ' ') {
        mpvKey(['cycle', 'pause']);
        e.preventDefault();
      } else if (e.key === 'ArrowLeft' && !e.altKey) {
        mpvKey(['seek', -10]);
        e.preventDefault();
      } else if (e.key === 'ArrowRight' && !e.altKey) {
        mpvKey(['seek', 10]);
        e.preventDefault();
      } else if (e.key === 'ArrowUp') {
        mpvKey(['add', 'volume', 5]);
        e.preventDefault();
      } else if (e.key === 'ArrowDown') {
        mpvKey(['add', 'volume', -5]);
        e.preventDefault();
      } else if (e.key === 'f' || e.key === 'F') {
        togglePageFullscreen();
        e.preventDefault();
      }
      return;
    }

    // Alt + 左箭头 = 上一集
    if (e.altKey && e.key === 'ArrowLeft') {
      if (detailRef.current && currentEpisodeIndexRef.current > 0) {
        handlePreviousEpisode();
        e.preventDefault();
      }
    }

    // Alt + 右箭头 = 下一集
    if (e.altKey && e.key === 'ArrowRight') {
      const d = detailRef.current;
      const idx = currentEpisodeIndexRef.current;
      if (d && idx < d.episodes.length - 1) {
        handleNextEpisode();
        e.preventDefault();
      }
    }

    // 左箭头 = 快退
    if (!e.altKey && e.key === 'ArrowLeft') {
      if (plyrRef.current && plyrRef.current.currentTime > 5) {
        plyrRef.current.currentTime -= 10;
        e.preventDefault();
      }
    }

    // 右箭头 = 快进
    if (!e.altKey && e.key === 'ArrowRight') {
      if (
        plyrRef.current &&
        plyrRef.current.currentTime < plyrRef.current.duration - 5
      ) {
        plyrRef.current.currentTime += 10;
        e.preventDefault();
      }
    }

    // 上箭头 = 音量+(100% 后继续放大,上限 300%)
    if (e.key === 'ArrowUp') {
      if (plyrRef.current) {
        changeVolume(VOLUME_STEP);
        e.preventDefault();
      }
    }

    // 下箭头 = 音量-
    if (e.key === 'ArrowDown') {
      if (plyrRef.current) {
        changeVolume(-VOLUME_STEP);
        e.preventDefault();
      }
    }

    // 空格 = 播放/暂停
    if (e.key === ' ') {
      if (plyrRef.current) {
        plyrRef.current.togglePlay();
        e.preventDefault();
      }
    }

    // f 键 = 切换全屏
    if (e.key === 'f' || e.key === 'F') {
      if (plyrRef.current) {
        plyrRef.current.fullscreen.toggle();
        e.preventDefault();
      }
    }
  };

  // ---------------------------------------------------------------------------
  // 播放记录相关
  // ---------------------------------------------------------------------------
  // 保存播放进度; mpv 嵌入模式下传入 mpv 的时间/时长 (Plyr 未接管)
  const saveCurrentPlayProgress = async (mpv?: {
    time: number;
    duration: number;
  }) => {
    if (!currentSourceRef.current || !currentIdRef.current) {
      return;
    }
    if (!mpv && !plyrRef.current) {
      return;
    }

    const player = plyrRef.current;
    const currentTime = mpv ? mpv.time : player?.currentTime || 0;
    const duration = mpv ? mpv.duration : player?.duration || 0;

    try {
      const saved = await invoke<boolean>('save_play_progress', {
        request: {
          source: currentSourceRef.current,
          id: currentIdRef.current,
          title: videoTitleRef.current,
          sourceName: detailRef.current?.source_name || '',
          year: detailRef.current?.year || '',
          cover: detailRef.current?.poster || '',
          episodeIndex: currentEpisodeIndexRef.current,
          totalEpisodes: detailRef.current?.episodes.length || 1,
          playTime: currentTime,
          totalTime: duration,
          searchTitle: searchTitle || '',
        },
      });

      if (!saved) {
        return;
      }

      // Notify other components

      lastSaveTimeRef.current = Date.now();
      console.log('Play progress saved:', {
        title: videoTitleRef.current,
        episode: currentEpisodeIndexRef.current + 1,
        year: detailRef.current?.year,
        progress: `${Math.floor(currentTime)}/${Math.floor(duration)}`,
      });
    } catch (err) {
      console.error('Failed to save play progress:', err);
    }
  };

  useEffect(() => {
    // 页面即将卸载时保存播放进度和清理资源
    const handleBeforeUnload = () => {
      saveCurrentPlayProgress();
      releaseWakeLock();
      cleanupPlayer();
    };

    // 页面可见性变化时保存播放进度和释放 Wake Lock
    const handleVisibilityChange = () => {
      if (document.visibilityState === 'hidden') {
        saveCurrentPlayProgress();
        releaseWakeLock();
      } else if (document.visibilityState === 'visible') {
        // 页面可见时保存播放进度和请求 Wake Lock
        if (plyrRef.current && !plyrRef.current.paused) {
          requestWakeLock();
        }
      }
    };

    // 添加事件监听器
    window.addEventListener('beforeunload', handleBeforeUnload);
    document.addEventListener('visibilitychange', handleVisibilityChange);

    return () => {
      // 清理事件监听器
      window.removeEventListener('beforeunload', handleBeforeUnload);
      document.removeEventListener('visibilitychange', handleVisibilityChange);
    };
  }, [currentEpisodeIndex, detail, plyrRef.current]);

  // ---------------------------------------------------------------------------
  // mpv 嵌入模式: 事件回传 / 子窗口 rect 同步 / 播完连播 / 进度与跳过
  // ---------------------------------------------------------------------------
  useEffect(() => {
    if (!mpvState.active) return;
    let disposed = false;
    const unlisten = listen<{
      kind: 'time' | 'duration' | 'pause' | 'eof' | 'file-loaded' | 'dead';
      time?: number;
      duration?: number;
      value?: boolean;
    }>('mpv-embed-event', (e) => {
      if (disposed) return;
      const p = e.payload;
      if (p.kind === 'time') {
        setMpvState((s) =>
          s.active
            ? {
                ...s,
                time: p.time ?? s.time,
                duration: p.duration || s.duration,
              }
            : s,
        );
      } else if (p.kind === 'duration') {
        setMpvState((s) => ({ ...s, duration: p.duration || s.duration }));
      } else if (p.kind === 'pause') {
        setMpvState((s) => ({ ...s, paused: Boolean(p.value) }));
      } else if (p.kind === 'eof') {
        setMpvState((s) => ({ ...s, eof: true }));
      } else if (p.kind === 'file-loaded') {
        setIsVideoLoading(false);
      } else if (p.kind === 'dead') {
        // 主动退出(close → quit)也会触发 dead, 此时 mpvActiveRef 已为 false
        if (!mpvActiveRef.current) return;
        showToast('mpv 播放器已退出', 'info');
        void exitMpvMode(true);
      }
    });
    return () => {
      disposed = true;
      void unlisten.then((f) => f());
    };
  }, [mpvState.active]);

  // mpv 子窗口 rect 同步: 视频区 rect 扣除底部控制条高度, CSS 坐标 × DPR。
  // resize/scroll 会高频触发, 拖拽窗口时每帧一次; 同步走 rAF 合帧 +
  // 尺寸去重, 避免 IPC 风暴拖累主线程。
  useEffect(() => {
    if (!mpvState.active) return;
    let raf = 0;
    let last = '';
    const sync = () => {
      const el = playerContainerRef.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      const key = `${r.left},${r.top},${r.width},${r.height},${
        window.devicePixelRatio || 1
      }`;
      if (key === last) return;
      last = key;
      void invoke('mpv_embed_sync', {
        x: r.left,
        y: r.top,
        w: r.width,
        h: Math.max(0, r.height - MPV_CONTROL_BAR_H),
        scale: window.devicePixelRatio || 1,
      }).catch(() => {});
    };
    const scheduleSync = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        sync();
      });
    };
    scheduleSync();
    const el = playerContainerRef.current;
    const ro = new ResizeObserver(scheduleSync);
    if (el) ro.observe(el);
    window.addEventListener('resize', scheduleSync);
    window.addEventListener('scroll', scheduleSync, true);
    return () => {
      if (raf) cancelAnimationFrame(raf);
      ro.disconnect();
      window.removeEventListener('resize', scheduleSync);
      window.removeEventListener('scroll', scheduleSync, true);
    };
  }, [mpvState.active, isPageFullscreen]);

  // 播完连播: eof-reached → 下一集 / 提示最后一集
  useEffect(() => {
    if (!mpvState.active || !mpvState.eof) return;
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    if (d && d.episodes && idx < d.episodes.length - 1) {
      setMpvState((s) => ({ ...s, eof: false }));
      setTimeout(() => setCurrentEpisodeIndex(idx + 1), 500);
    } else {
      showToast('已是最后一集', 'info');
    }
  }, [mpvState.eof, mpvState.active]);

  // 进度保存 + 跳过片头片尾: 复用 player_tick 的节流决策, 控制经 IPC 下发
  useEffect(() => {
    if (!mpvState.active || mpvState.paused) return;
    const timer = setInterval(async () => {
      const st = mpvStateRef.current;
      if (!st.active || st.paused || st.duration <= 0) return;
      const detailData = detailRef.current;
      const idx = currentEpisodeIndexRef.current;
      try {
        const tickDecision = await invoke<PlayerTickDecision>('player_tick', {
          request: {
            currentTime: st.time,
            totalDuration: st.duration,
            nowMs: Date.now(),
            lastSaveAtMs: lastSaveTimeRef.current,
            saveIntervalMs: 5000,
            lastSkipCheckAtMs: lastSkipCheckRef.current,
            skipEnabled: skipConfigRef.current.enable,
            introTime: skipConfigRef.current.intro_time,
            outroTime: Math.abs(skipConfigRef.current.outro_time),
            source: detailData?.source || null,
            id: detailData?.id || null,
            currentEpisode: detailData?.episodes ? idx : null,
            totalEpisodes: detailData?.episodes?.length || null,
          },
        });
        lastSaveTimeRef.current = tickDecision.nextLastSaveAtMs;
        lastSkipCheckRef.current = tickDecision.nextLastSkipCheckAtMs;
        if (tickDecision.shouldSaveProgress) {
          void saveCurrentPlayProgress({
            time: st.time,
            duration: st.duration,
          });
        }
        const skipAction = tickDecision.skipAction;
        if (
          skipAction &&
          typeof skipAction === 'object' &&
          'SkipIntro' in skipAction &&
          st.time > 0.5
        ) {
          const targetTime = skipAction.SkipIntro;
          await invoke('mpv_embed_command', {
            cmd: ['seek', targetTime, 'absolute'],
          });
          showToast(`跳过片头，跳转到 ${formatTime(targetTime)}`, 'success');
        } else if (skipAction === 'SkipOutro' && st.time < st.duration - 1) {
          if (
            currentEpisodeIndexRef.current <
            (detailRef.current?.episodes?.length || 1) - 1
          ) {
            showToast('跳过片尾，跳转到下一集', 'info');
            setTimeout(() => handleNextEpisode(), 500);
          } else {
            await invoke('mpv_embed_command', {
              cmd: ['set_property', 'pause', true],
            });
            showToast('跳过片尾，但当前已是最后一集', 'info');
          }
        }
      } catch {
        /* player_tick 失败静默: 下一轮重试 */
      }
    }, 1000);
    return () => clearInterval(timer);
  }, [mpvState.active, mpvState.paused]);

  // 清理定时器
  useEffect(() => {
    return () => {
      if (saveIntervalRef.current) {
        clearInterval(saveIntervalRef.current);
      }
    };
  }, []);

  // ---------------------------------------------------------------------------
  // 收藏相关
  // ---------------------------------------------------------------------------
  const refreshCurrentFavoriteStatus = async (
    source: string,
    id: string,
  ): Promise<void> => {
    try {
      const key = generateStorageKey(source, id);
      const statuses = await invoke<Record<string, boolean>>(
        'get_play_favorite_statuses',
        {
          keys: [key],
        },
      );
      setFavorited(Boolean(statuses[key]));
    } catch (err) {
      console.error('检查收藏状态失败:', err);
    }
  };

  // 每当 source 或 id 变化时检查收藏状态
  useEffect(() => {
    if (!currentSource || !currentId) return;
    refreshCurrentFavoriteStatus(currentSource, currentId);
  }, [currentSource, currentId]);

  // 监听收藏数据更新事件
  useEffect(() => {
    if (!currentSource || !currentId) return;

    const unsubscribe = subscribeToDataUpdates('favoritesUpdated', async () => {
      await refreshCurrentFavoriteStatus(currentSource, currentId);
    });

    return unsubscribe;
  }, [currentSource, currentId]);

  // 切换收藏
  const handleToggleFavorite = async () => {
    if (
      !videoTitleRef.current ||
      !detailRef.current ||
      !currentSourceRef.current ||
      !currentIdRef.current
    )
      return;

    try {
      const key = generateStorageKey(
        currentSourceRef.current,
        currentIdRef.current,
      );
      const response = await invoke<{ favorited: boolean }>(
        'toggle_play_favorite',
        {
          record: {
            key,
            title: videoTitleRef.current,
            source_name: detailRef.current?.source_name || '',
            year: detailRef.current?.year || '',
            cover: detailRef.current?.poster || '',
            episode_index: currentEpisodeIndexRef.current + 1,
            total_episodes: detailRef.current?.episodes.length || 1,
            save_time: Math.floor(Date.now() / 1000),
            search_title: searchTitle || '',
          },
        },
      );
      setFavorited(response.favorited);
    } catch (err) {
      console.error('切换收藏失败:', err);
    }
  };

  useEffect(() => {
    if (
      !videoUrl ||
      loading ||
      currentEpisodeIndex === null ||
      !playerContainerRef.current
    ) {
      return;
    }

    if (
      !detail ||
      !detail.episodes ||
      currentEpisodeIndex >= detail.episodes.length ||
      currentEpisodeIndex < 0
    ) {
      setError(`选集索引无效，当前共 ${totalEpisodes} 集`);
      return;
    }

    // mpv 嵌入模式: 视频由原生子窗口接管, Plyr 不加载。
    // m3u8 源 WebView2 能放 → 自动切回内置播放器。
    if (mpvActiveRef.current) {
      if (isMsePlayableUrl(videoUrl)) {
        void exitMpvMode(true);
      } else {
        void launchMpvExternal(videoUrl, currentEpisodeTitle());
      }
      return;
    }

    // 音量增强会接管 <video> 的音频输出且无法解绑;若上一集开了增强(走 MSE)
    // 而新一集为直连源(无法走 MSE),必须重建播放器,避免直连源在音频图里静音。
    if (boostGraphRef.current && !isMsePlayableUrl(videoUrl)) {
      cleanupPlayer();
    }

    const loadSource = (video: HTMLVideoElement, url: string) => {
      if (!url) return;

      if (hlsRef.current) {
        hlsRef.current.destroy();
        hlsRef.current = null;
      }
      if (video.hls) {
        video.hls.destroy();
        delete video.hls;
      }

      const isM3u8 = /\.m3u8($|\?)/i.test(url);
      if (isM3u8 && Hls.isSupported()) {
        const hls = new Hls({
          debug: false,
          enableWorker: true,
          lowLatencyMode: false,
          backBufferLength: 90,
          maxBufferLength: 120,
          maxMaxBufferLength: 240,
          maxBufferSize: 300 * 1000 * 1000,
          maxBufferHole: 0.8,
          maxFragLookUpTolerance: 0.5,
          nudgeOffset: 0.1,
          nudgeMaxRetry: 5,
          startLevel: -1,
          autoStartLoad: true,
          startPosition: -1,
          progressive: true,
          abrEwmaDefaultEstimate: 300000,
          abrBandWidthFactor: 0.85,
          abrBandWidthUpFactor: 0.6,
          abrEwmaFastLive: 2.0,
          abrEwmaSlowLive: 6.0,
          fragLoadingTimeOut: 25000,
          fragLoadingMaxRetry: 6,
          fragLoadingRetryDelay: 500,
          fragLoadingMaxRetryTimeout: 8000,
          manifestLoadingTimeOut: 15000,
          manifestLoadingMaxRetry: 4,
          manifestLoadingRetryDelay: 500,
          manifestLoadingMaxRetryTimeout: 8000,
          levelLoadingTimeOut: 15000,
          levelLoadingMaxRetry: 4,
          levelLoadingRetryDelay: 500,
          levelLoadingMaxRetryTimeout: 8000,
          loader: TauriHlsJsLoader,
          enableAdBlock: blockAdEnabledRef.current,
        } as any);

        hls.loadSource(url);
        hls.attachMedia(video);
        hlsRef.current = hls;
        video.hls = hls;

        hls.on(Hls.Events.FRAG_LOADED, (_e: any, data: any) => {
          // 实时下载速度: 累计分片字节, 由 1s 定时器换算
          const loaded = Number(data?.frag?.stats?.loaded ?? 0);
          if (loaded > 0) {
            netSpeedWindowRef.current.bytes += loaded;
          }
        });

        hls.on(Hls.Events.ERROR, function (_event: any, data: any) {
          console.warn(
            `[播放] hls 错误: fatal=${data?.fatal} type=${data?.type} details=${data?.details} url=${String(data?.url ?? '').slice(0, 120)}`,
          );
          if (!data?.fatal) return;
          switch (data.type) {
            case Hls.ErrorTypes.NETWORK_ERROR:
              hls.startLoad();
              break;
            case Hls.ErrorTypes.MEDIA_ERROR:
              hls.recoverMediaError();
              break;
            default:
              hls.destroy();
              break;
          }
        });
      } else {
        video.src = url;
        // mp4 直链: 无 HLS 分片统计; webkitVideoDecodedByteCount 是累计值, 取窗口增量
        console.log(`[播放] mp4/直链模式加载: url=${url.slice(0, 120)}`);
        let lastDecoded = Number(
          (video as any).webkitVideoDecodedByteCount ?? 0,
        );
        const mp4Progress = () => {
          const decoded = Number(
            (video as any).webkitVideoDecodedByteCount ?? 0,
          );
          if (decoded > lastDecoded) {
            netSpeedWindowRef.current.bytes += decoded - lastDecoded;
            lastDecoded = decoded;
          }
        };
        video.removeEventListener('progress', mp4Progress);
        video.addEventListener('progress', mp4Progress);
      }

      ensureVideoSource(video, url);
      video.load();
      video.onerror = () => {
        console.error(
          `[播放] video 元素错误: code=${(video.error as MediaError | null)?.code} message=${(video.error as MediaError | null)?.message} url=${url.slice(0, 120)}`,
        );
        // 本地网盘代理地址失败: 大概率 dlink 签名过期(403) → 重新解析换新链, 限 1 次
        if (url.includes('/netdisk/file.mp4?')) {
          const retry = directRetryRef.current;
          if (retry.url === url || retry.count >= 1) {
            console.warn('[播放] 网盘直链重试已用尽或同链重试, 放弃');
            return;
          }
          directRetryRef.current = { url, count: retry.count + 1 };
          console.log('[播放] 网盘直链失败, 重新解析换链…');
          const detailData = detailRef.current;
          const idx = currentEpisodeIndexRef.current;
          if (detailData) {
            void (async () => {
              setIsVideoLoading(true);
              const fresh = await resolveEpisodeUrl(detailData, idx);
              if (fresh) {
                setVideoUrl(fresh);
              } else {
                setIsVideoLoading(false);
              }
            })();
          }
        }
      };
      // HEVC 黑屏检测: 数据能加载(loadeddata)但视频轨解不出来(videoWidth===0),
      // 典型于 WebView2 无 HEVC 扩展播放网盘 HEVC-MKV → 自动换 mpv
      const hevcFallback = () => {
        if (!url.includes('/netdisk/file.mp4?')) return;
        if (video.videoWidth > 0) return; // 有画面, 不是解码问题
        if (video.readyState < 2) return; // 数据还没到, 不判断
        console.warn(
          `[播放] 检测到黑屏有声(readyState=${video.readyState} videoWidth=0) → HEVC 兜底切 mpv`,
        );
        const detailData = detailRef.current;
        const idx = currentEpisodeIndexRef.current;
        const epName =
          detailData?.episodes_titles?.[idx] ||
          `${detailData?.title || '视频'} 第${idx + 1}集`;
        void launchMpvExternal(url, epName);
      };
      video.addEventListener('loadeddata', hevcFallback, { once: true });
      // loadeddata 后再给 2s 窗口确认 videoWidth 仍为 0 (部分容器元数据晚到)
      const hevcTimer = window.setTimeout(() => hevcFallback(), 2000);
      video.addEventListener(
        'loadedmetadata',
        () => {
          if (video.videoWidth > 0) window.clearTimeout(hevcTimer);
        },
        { once: true },
      );

      // Plyr 的 autoplay 仅在播放器实例创建时生效一次，后续选集换源后
      // 需要重新在 canplay 时触发一次播放
      if (lastStartedUrlRef.current !== url) {
        lastStartedUrlRef.current = url;
        const resumePlay = () => {
          if (video.oncanplay === resumePlay) {
            video.oncanplay = null;
          }
          const playResult = video.play();
          if (playResult && typeof playResult.catch === 'function') {
            playResult.catch(() => {
              /* 自动播放被浏览器策略拦截时静默忽略 */
            });
          }
        };
        video.oncanplay = resumePlay;
      }
    };

    let cancelled = false;

    const initPlyr = async () => {
      try {
        const { default: PlyrConstructor } = await import('plyr');
        if (cancelled || !playerContainerRef.current) return;

        let video = videoElementRef.current;
        let player = plyrRef.current;

        if (!video) {
          if (typeof document === 'undefined') return;
          video = document.createElement('video');
          const posterUrl = videoCover || '/logo.png';
          video.className = 'quantum-plyr-video';
          video.poster = posterUrl;
          video.setAttribute('poster', posterUrl);
          // Do not force CORS mode for poster/media requests. Many third-party
          // image hosts do not return ACAO and would be blocked in WebView/browser.
          video.removeAttribute('crossorigin');
          video.playsInline = true;
          video.controls = true;
          video.disableRemotePlayback = false;
          playerContainerRef.current.innerHTML = '';
          playerContainerRef.current.appendChild(video);
          videoElementRef.current = video;
        }

        if (!player) {
          const isTouchDevice =
            typeof window !== 'undefined' &&
            (window.matchMedia?.('(pointer: coarse)')?.matches ||
              'ontouchstart' in window ||
              navigator.maxTouchPoints > 0);
          player = new PlyrConstructor(video, {
            autoplay: true,
            muted: false,
            volume: lastVolumeRef.current,
            seekTime: 10,
            // 单击视频画面切换播放/暂停(触屏设备由自建手势层处理, 避免与滑动快进冲突)
            clickToPlay: true,
            resetOnEnd: false,
            fullscreen: {
              enabled: true,
              fallback: true,
              iosNative: false,
            },
            keyboard: {
              focused: false,
              global: false,
            },
            speed: {
              selected: lastPlaybackRateRef.current,
              options: [0.5, 0.75, 1, 1.25, 1.5, 2, 3],
            },
            controls: [
              'play-large',
              'play',
              'progress',
              'current-time',
              'duration',
              'mute',
              'volume',
              'settings',
              // 不启用 'pip'：WebView2 的画中画小窗自带原生齿轮按钮，
              // 点击会跳转 edge://settings/...（WebView2 无浏览器内部页面），
              // 导致"无法访问此页面"顶掉应用 UI。改用下方自建的"页面全屏"。
              'airplay',
              'fullscreen',
            ],
            settings: ['speed', 'loop'],
            i18n: {
              speed: '速度',
              normal: '正常',
              settings: '设置',
              disabled: '关闭',
              enabled: '开启',
            },
          });

          plyrRef.current = player;

          player.on('ready', () => {
            setError(null);
            enhancePlyrUi();
            if (!player!.paused) {
              requestWakeLock();
            }
          });

          player.on('play', () => {
            requestWakeLock();
          });

          player.on('pause', () => {
            releaseWakeLock();
            saveCurrentPlayProgress();
          });

          player.on('ended', () => {
            releaseWakeLock();
            const d = detailRef.current;
            const idx = currentEpisodeIndexRef.current;
            if (d && d.episodes && idx < d.episodes.length - 1) {
              setTimeout(() => {
                setCurrentEpisodeIndex(idx + 1);
              }, 1000);
            }
          });

          player.on('volumechange', () => {
            // 元素音量恒为 0..1;增强中(>100%)把音量条拖回 <100% 视为取消增强
            const elementVolume = player!.volume;
            if (lastVolumeRef.current > 1) {
              if (elementVolume < 0.999) {
                lastVolumeRef.current = elementVolume;
                setBoostLevel(null);
                if (boostGraphRef.current) {
                  boostGraphRef.current.gain.gain.value = 1;
                }
              }
            } else {
              lastVolumeRef.current = elementVolume;
            }
            persistVolume(lastVolumeRef.current);
          });

          player.on('ratechange', () => {
            lastPlaybackRateRef.current = player!.speed;
          });

          player.on('canplay', () => {
            if (resumeTimeRef.current && resumeTimeRef.current > 0) {
              try {
                const duration = player!.duration || 0;
                let target = resumeTimeRef.current;
                if (duration && target >= duration - 2) {
                  target = Math.max(0, duration - 5);
                }
                player!.currentTime = target;
              } catch (err) {
                console.warn('设置播放位置失败:', err);
              }
            }

            resumeTimeRef.current = null;
            setTimeout(() => {
              applyVolume(lastVolumeRef.current);
              if (
                Math.abs(player!.speed - lastPlaybackRateRef.current) > 0.01
              ) {
                player!.speed = lastPlaybackRateRef.current;
              }
            }, 0);

            setIsVideoLoading(false);
          });

          player.on('timeupdate', async () => {
            const currentTime = player!.currentTime || 0;
            const duration = player!.duration || 0;
            const now = Date.now();

            let interval = 5000;
            if (process.env.NEXT_PUBLIC_STORAGE_TYPE === 'upstash') {
              interval = 20000;
            }
            const detail = detailRef.current;
            const currentIdx = currentEpisodeIndexRef.current;
            try {
              const tickDecision = await invoke<PlayerTickDecision>(
                'player_tick',
                {
                  request: {
                    currentTime,
                    totalDuration: duration,
                    nowMs: now,
                    lastSaveAtMs: lastSaveTimeRef.current,
                    saveIntervalMs: interval,
                    lastSkipCheckAtMs: lastSkipCheckRef.current,
                    skipEnabled: skipConfigRef.current.enable,
                    introTime: skipConfigRef.current.intro_time,
                    outroTime: Math.abs(skipConfigRef.current.outro_time),
                    source: detail?.source || null,
                    id: detail?.id || null,
                    currentEpisode:
                      detail && detail.episodes ? currentIdx : null,
                    totalEpisodes: detail?.episodes?.length || null,
                  },
                },
              );

              lastSaveTimeRef.current = tickDecision.nextLastSaveAtMs;
              lastSkipCheckRef.current = tickDecision.nextLastSkipCheckAtMs;

              if (tickDecision.shouldSaveProgress) {
                saveCurrentPlayProgress();
              }

              const skipAction = tickDecision.skipAction;
              if (
                skipAction &&
                typeof skipAction === 'object' &&
                'SkipIntro' in skipAction &&
                currentTime > 0.5
              ) {
                const targetTime = skipAction.SkipIntro;
                player!.currentTime = targetTime;
                showToast(
                  `跳过片头，跳转到 ${formatTime(targetTime)}`,
                  'success',
                );
              } else if (
                skipAction === 'SkipOutro' &&
                currentTime < duration - 1
              ) {
                if (
                  currentEpisodeIndexRef.current <
                  (detailRef.current?.episodes?.length || 1) - 1
                ) {
                  showToast('跳过片尾，跳转到下一集', 'info');
                  setTimeout(() => {
                    handleNextEpisode();
                  }, 500);
                } else {
                  showToast('跳过片尾，但当前已是最后一集', 'info');
                  player!.pause();
                }
              }

              if (tickDecision.didPreload) {
                const stats =
                  await invoke<
                    Record<
                      string,
                      { entry_count: number; weighted_size: number }
                    >
                  >('get_cache_stats');
                console.log(
                  '📊 预载后缓存统计 | 视频缓存:',
                  stats.video.entry_count,
                  '条 | 搜索缓存:',
                  stats.search.entry_count,
                  '条',
                );
              }
            } catch (err) {
              console.error('player_tick 执行失败:', err);
            }
          });

          player.on('error', (err: any) => {
            console.error('播放器错误', err);
            if ((player?.currentTime || 0) <= 0) {
              setError('无法播放');
            }
          });

          if (!player.paused) {
            requestWakeLock();
          }
        }

        if (cancelled) return;
        const posterUrl = videoCover || '/logo.png';
        video.removeAttribute('crossorigin');
        video.poster = posterUrl;
        video.setAttribute('poster', posterUrl);
        player.poster = posterUrl;
        setIsVideoLoading(true);
        loadSource(video, videoUrl);
        setTimeout(() => {
          enhancePlyrUi();
        }, 0);
      } catch (err) {
        console.error('创建播放器失败:', err);
        setError('播放器初始化失败');
      }
    };

    void initPlyr();

    // 实时速度换算定时器: 每 1s 把窗口内累计字节换算成 B/s
    // 徽章仅在速度明显变化(>15% 或从 0 恢复)时弹出, 3s 无更新自动隐藏
    const speedTimer = setInterval(() => {
      const now = Date.now();
      const win = netSpeedWindowRef.current;
      const elapsed = (now - win.at) / 1000;
      if (elapsed <= 0) return;
      const bps = win.bytes / elapsed;
      win.bytes = 0;
      win.at = now;

      const lastShown = lastShownBpsRef.current;
      const changed =
        bps > 0 &&
        (lastShown === 0 || bps > lastShown * 1.15 || bps < lastShown * 0.85);
      if (changed) {
        lastShownBpsRef.current = bps;
        showSpeedBadge(bps);
      } else if (bps === 0 && lastShown !== 0) {
        lastShownBpsRef.current = 0;
        if (speedHideTimerRef.current) {
          clearTimeout(speedHideTimerRef.current);
        }
        setSpeedBadge(null);
      }
    }, 1000);

    return () => {
      cancelled = true;
      clearInterval(speedTimer);
      if (speedHideTimerRef.current) {
        clearTimeout(speedHideTimerRef.current);
      }
    };
  }, [
    videoUrl,
    loading,
    currentEpisodeIndex,
    detail,
    totalEpisodes,
    videoCover,
    blockAdEnabled,
    plyrReloadTick,
  ]);

  // 当组件卸载时清理定时器、Wake Lock 和播放器资源
  useEffect(() => {
    return () => {
      // 清理定时器
      if (saveIntervalRef.current) {
        clearInterval(saveIntervalRef.current);
      }
      if (swipeSeekOverlayTimerRef.current) {
        clearTimeout(swipeSeekOverlayTimerRef.current);
        swipeSeekOverlayTimerRef.current = null;
      }

      // 释放 Wake Lock
      releaseWakeLock();

      // 销毁播放器实例
      cleanupPlayer();
    };
  }, []);

  if (loading) {
    return (
      <PageLayout activePath='/play'>
        <div className='flex items-center justify-center min-h-screen bg-transparent'>
          <div className='text-center max-w-md mx-auto px-6'>
            {/* 动画影院图标 */}
            <div className='relative mb-8'>
              <div className='relative mx-auto w-24 h-24 bg-linear-to-r from-green-500 to-emerald-600 rounded-2xl shadow-2xl flex items-center justify-center transform hover:scale-105 transition-transform duration-300'>
                <div className='text-white text-4xl'>
                  {loadingStage === 'searching' && '🔍'}
                  {loadingStage === 'preferring' && '⚡'}
                  {loadingStage === 'fetching' && '🎬'}
                  {loadingStage === 'ready' && '✨'}
                </div>
                {/* 旋转光环 */}
                <div className='absolute -inset-2 bg-linear-to-r from-green-500 to-emerald-600 rounded-2xl opacity-20 animate-spin'></div>
              </div>

              {/* 浮动粒子效果 */}
              <div className='absolute top-0 left-0 w-full h-full pointer-events-none'>
                <div className='absolute top-2 left-2 w-2 h-2 bg-green-400 rounded-full animate-bounce'></div>
                <div
                  className='absolute top-4 right-4 w-1.5 h-1.5 bg-emerald-400 rounded-full animate-bounce'
                  style={{ animationDelay: '0.5s' }}
                ></div>
                <div
                  className='absolute bottom-3 left-6 w-1 h-1 bg-lime-400 rounded-full animate-bounce'
                  style={{ animationDelay: '1s' }}
                ></div>
              </div>
            </div>

            {/* 进度指示器 */}
            <div className='mb-6 w-80 mx-auto'>
              <div className='flex justify-center space-x-2 mb-4'>
                <div
                  className={`w-3 h-3 rounded-full transition-all duration-500 ${
                    loadingStage === 'searching' || loadingStage === 'fetching'
                      ? 'bg-green-500 scale-125'
                      : loadingStage === 'preferring' ||
                          loadingStage === 'ready'
                        ? 'bg-green-500'
                        : 'bg-gray-300'
                  }`}
                ></div>
                <div
                  className={`w-3 h-3 rounded-full transition-all duration-500 ${
                    loadingStage === 'preferring'
                      ? 'bg-green-500 scale-125'
                      : loadingStage === 'ready'
                        ? 'bg-green-500'
                        : 'bg-gray-300'
                  }`}
                ></div>
                <div
                  className={`w-3 h-3 rounded-full transition-all duration-500 ${
                    loadingStage === 'ready'
                      ? 'bg-green-500 scale-125'
                      : 'bg-gray-300'
                  }`}
                ></div>
              </div>

              {/* 进度条 */}
              <div className='w-full bg-gray-200 dark:bg-gray-700 rounded-full h-2 overflow-hidden'>
                <div
                  className='h-full bg-linear-to-r from-green-500 to-emerald-600 rounded-full transition-all duration-1000 ease-out'
                  style={{
                    width:
                      loadingStage === 'searching' ||
                      loadingStage === 'fetching'
                        ? '33%'
                        : loadingStage === 'preferring'
                          ? '66%'
                          : '100%',
                  }}
                ></div>
              </div>
            </div>

            {/* 加载消息 */}
            <div className='space-y-2'>
              <p className='text-xl font-semibold text-gray-800 dark:text-gray-200 animate-pulse'>
                {loadingMessage}
              </p>
            </div>
          </div>
        </div>
      </PageLayout>
    );
  }

  if (error) {
    return (
      <PageLayout activePath='/play'>
        <div className='flex items-center justify-center min-h-screen bg-transparent'>
          <div className='text-center max-w-md mx-auto px-6'>
            {/* 错误图标 */}
            <div className='relative mb-8'>
              <div className='relative mx-auto w-24 h-24 bg-linear-to-r from-red-500 to-orange-500 rounded-2xl shadow-2xl flex items-center justify-center transform hover:scale-105 transition-transform duration-300'>
                <div className='text-white text-4xl'>😵</div>
                {/* 脉冲效果 */}
                <div className='absolute -inset-2 bg-linear-to-r from-red-500 to-orange-500 rounded-2xl opacity-20 animate-pulse'></div>
              </div>

              {/* 浮动错误粒子 */}
              <div className='absolute top-0 left-0 w-full h-full pointer-events-none'>
                <div className='absolute top-2 left-2 w-2 h-2 bg-red-400 rounded-full animate-bounce'></div>
                <div
                  className='absolute top-4 right-4 w-1.5 h-1.5 bg-orange-400 rounded-full animate-bounce'
                  style={{ animationDelay: '0.5s' }}
                ></div>
                <div
                  className='absolute bottom-3 left-6 w-1 h-1 bg-yellow-400 rounded-full animate-bounce'
                  style={{ animationDelay: '1s' }}
                ></div>
              </div>
            </div>

            {/* 错误信息 */}
            <div className='space-y-4 mb-8'>
              <h2 className='text-2xl font-bold text-gray-800 dark:text-gray-200'>
                哎呀，出现了一些问题
              </h2>
              <div className='bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg p-4'>
                <p className='text-red-600 dark:text-red-400 font-medium'>
                  {error}
                </p>
              </div>
              <p className='text-sm text-gray-500 dark:text-gray-400'>
                请检查网络连接或尝试刷新页面
              </p>
            </div>

            {/* 操作按钮 */}
            <div className='space-y-3'>
              <button
                onClick={() =>
                  videoTitle
                    ? router.push(`/search?q=${encodeURIComponent(videoTitle)}`)
                    : router.back()
                }
                className='w-full px-6 py-3 bg-linear-to-r from-green-500 to-emerald-600 text-white rounded-xl font-medium hover:from-green-600 hover:to-emerald-700 transform hover:scale-105 transition-all duration-200 shadow-lg hover:shadow-xl'
              >
                {videoTitle ? '🔍 返回搜索' : '← 返回上页'}
              </button>

              <button
                onClick={() => window.location.reload()}
                className='w-full px-6 py-3 bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-300 rounded-xl font-medium hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors duration-200'
              >
                🔄 重新尝试
              </button>
            </div>
          </div>
        </div>
      </PageLayout>
    );
  }

  const swipeSeekOverlayNode = swipeSeekOverlay ? (
    <div className='absolute inset-0 z-40 pointer-events-none overflow-hidden'>
      <div
        className={`absolute inset-y-0 w-1/2 ${
          swipeSeekOverlay.direction === 'forward'
            ? 'right-0 bg-gradient-to-l from-emerald-500/35 via-emerald-500/15 to-transparent'
            : 'left-0 bg-gradient-to-r from-orange-500/35 via-orange-500/15 to-transparent'
        }`}
      />
      <div
        className={`absolute top-1/2 -translate-y-1/2 ${
          swipeSeekOverlay.direction === 'forward' ? 'right-[8%]' : 'left-[8%]'
        }`}
      >
        <div className='flex min-w-[8.5rem] max-w-[85vw] flex-col items-center gap-1.5 rounded-2xl border border-white/25 bg-black/70 px-4 py-3 text-white shadow-2xl backdrop-blur-md'>
          <div className='flex items-center gap-1.5 text-sm font-semibold sm:text-base'>
            {swipeSeekOverlay.direction === 'forward' ? (
              <FastForward className='h-5 w-5 text-emerald-300' />
            ) : (
              <Rewind className='h-5 w-5 text-orange-300' />
            )}
            <span>{Math.round(swipeSeekOverlay.seconds)} 秒</span>
          </div>
          <div className='text-[11px] text-white/85 sm:text-xs'>
            {swipeSeekOverlay.direction === 'forward' ? '快进到' : '快退到'}{' '}
            {formatTime(swipeSeekOverlay.targetTime)}
          </div>
        </div>
      </div>
    </div>
  ) : null;

  return (
    <PageLayout activePath='/play'>
      <div
        className={`${appLayoutClasses.pageShell} flex flex-col gap-4 py-4 max-[375px]:py-3.5 min-[834px]:gap-5 min-[834px]:py-6 min-[1440px]:gap-6 min-[1440px]:py-8`}
      >
        {/* 第一行：影片标题 + 收藏 + 跳过设置 */}
        <div className='py-1 flex items-start justify-between gap-3 min-[834px]:gap-4'>
          <div className='min-w-0 flex-1'>
            <div className='flex items-center gap-2 min-w-0'>
              <h1 className='truncate text-2xl font-bold tracking-[-0.03em] text-gray-900 max-[375px]:text-xl sm:text-3xl lg:text-[2.25rem] xl:text-[2.5rem] min-[1440px]:text-[2.75rem] dark:text-gray-100'>
                {videoTitle || '影片标题'}
              </h1>
              <button
                type='button'
                onClick={(e) => {
                  e.stopPropagation();
                  handleToggleFavorite();
                }}
                className='tap-target shrink-0 transition-opacity hover:opacity-80'
                aria-label={favorited ? '取消收藏' : '添加收藏'}
              >
                <FavoriteIcon filled={favorited} />
              </button>
            </div>
            {totalEpisodes > 1 && (
              <div className='mt-1.5 truncate text-sm font-medium text-gray-500 max-[375px]:text-[0.84rem] sm:text-base lg:mt-2 lg:text-lg dark:text-gray-400'>
                {detail?.episodes_titles?.[currentEpisodeIndex] ||
                  `第 ${currentEpisodeIndex + 1} 集`}
              </div>
            )}
          </div>

          {/* 跳过设置按钮（断点驱动尺寸/视觉密度） */}
          <button
            type='button'
            onClick={() => setIsSkipConfigPanelOpen(true)}
            title='设置跳过片头片尾'
            className={cn(
              'tap-target group relative flex shrink-0 items-center gap-1.5 self-start rounded-full px-3.5 py-2 text-xs font-medium transition-all duration-200',
              'lg:rounded-xl lg:px-4 lg:py-2 lg:text-sm lg:shadow-md lg:hover:shadow-lg lg:hover:scale-105',
              skipConfig.enable
                ? 'bg-purple-100 text-purple-700 ring-1 ring-purple-500/30 dark:bg-purple-900/40 dark:text-purple-300 lg:bg-gradient-to-r lg:from-purple-600 lg:via-pink-500 lg:to-indigo-600 lg:text-white lg:ring-0'
                : 'bg-gray-100 text-gray-600 ring-1 ring-gray-500/10 dark:bg-gray-800 dark:text-gray-400 lg:bg-gradient-to-r lg:from-gray-100 lg:to-gray-200 lg:dark:from-gray-700 lg:dark:to-gray-800 lg:text-gray-700 lg:dark:text-gray-300 lg:ring-0',
            )}
          >
            <svg
              className='h-3.5 w-3.5 lg:h-5 lg:w-5'
              fill='none'
              stroke='currentColor'
              viewBox='0 0 24 24'
              aria-hidden='true'
            >
              <path
                strokeLinecap='round'
                strokeLinejoin='round'
                strokeWidth={2}
                d='M13 5l7 7-7 7M5 5l7 7-7 7'
              />
            </svg>
            <span>{skipConfig.enable ? '跳过已启用' : '跳过设置'}</span>
            {skipConfig.enable && (
              <span
                className='absolute -right-1 -top-1 hidden h-3 w-3 animate-pulse rounded-full bg-green-400 lg:block'
                aria-hidden='true'
              />
            )}
          </button>
        </div>
        {/* 第二行：播放器 + 右侧封面/描述（lg+） */}
        <div
          className={cn(
            'grid grid-cols-1 gap-3 transition-all duration-300 ease-in-out min-[834px]:gap-5',
            'lg:h-[68vh] lg:gap-6 xl:h-[72vh] min-[1440px]:h-[76vh] 2xl:h-[80vh]',
            'lg:grid-cols-[minmax(0,1fr)_minmax(0,20rem)] xl:grid-cols-[minmax(0,1fr)_minmax(0,24rem)] 2xl:grid-cols-[minmax(0,1fr)_minmax(0,28rem)]',
          )}
        >
          {/* 播放器壳：mobile/tablet 宽度驱动 16:9，lg+ 高度驱动 16:9 居中 */}
          <div className='min-h-0 h-full transition-all duration-300 ease-in-out lg:flex lg:items-center lg:justify-center'>
            <div
              className={cn(
                'group/player relative overflow-hidden bg-black shadow-lg',
                'rounded-[1.25rem] sm:rounded-[1.15rem]',
                // mobile/tablet portrait：贴边 + 宽度驱动
                '-mx-3 w-[calc(100%+1.5rem)] max-[375px]:-mx-2.5 max-[375px]:w-[calc(100%+1.25rem)] sm:mx-0 sm:w-full',
                // 始终保持 16:9
                'aspect-video',
                // lg+ 改为高度驱动，宽度由比例算出
                'lg:mx-0 lg:h-full lg:w-auto lg:max-w-full lg:max-h-full',
                // 页面全屏：铺满窗口（样式在 globals.css，用 !important 覆盖上面的响应式类）
                isPageFullscreen && 'quantum-page-fullscreen',
              )}
            >
              <div
                ref={playerContainerRef}
                className='quantum-plyr-shell absolute inset-0'
              />

              {/* 音量增强角标(>100% 时显示) */}
              {boostLevel !== null && boostLevel > 1 && (
                <div className='pointer-events-none absolute left-3 top-3 z-30 flex items-center gap-1.5 rounded-full bg-black/55 px-3 py-1.5 text-xs font-semibold text-amber-300 ring-1 ring-amber-400/40 backdrop-blur-md'>
                  <Volume2 className='h-3.5 w-3.5' aria-hidden='true' />
                  音量增强 ×{boostLevel.toFixed(1)}
                </div>
              )}

              {/* 悬浮折叠按钮（仅 lg+，hover 显形） */}
              <button
                type='button'
                onClick={() =>
                  setIsEpisodeSelectorCollapsed(!isEpisodeSelectorCollapsed)
                }
                title={
                  isEpisodeSelectorCollapsed ? '显示选集面板' : '隐藏选集面板'
                }
                aria-label={
                  isEpisodeSelectorCollapsed ? '显示选集面板' : '隐藏选集面板'
                }
                className={cn(
                  'absolute right-3 top-3 z-30 hidden items-center gap-1.5 rounded-full bg-black/55 px-3 py-1.5 text-xs font-medium text-white opacity-0 pointer-events-none ring-1 ring-white/20 backdrop-blur-md transition lg:flex',
                  'group-hover/player:opacity-100 group-hover/player:pointer-events-auto focus-visible:opacity-100 focus-visible:pointer-events-auto',
                  // 触屏设备无 hover：始终可见 + 加大触控区
                  '[@media(pointer:coarse)]:opacity-100 [@media(pointer:coarse)]:pointer-events-auto [@media(pointer:coarse)]:tap-target',
                )}
              >
                <svg
                  className={cn(
                    'h-3.5 w-3.5 transition-transform',
                    isEpisodeSelectorCollapsed && 'rotate-180',
                  )}
                  fill='none'
                  stroke='currentColor'
                  viewBox='0 0 24 24'
                  aria-hidden='true'
                >
                  <path
                    strokeLinecap='round'
                    strokeLinejoin='round'
                    strokeWidth='2'
                    d='M9 5l7 7-7 7'
                  />
                </svg>
                <span>{isEpisodeSelectorCollapsed ? '显示' : '隐藏'}</span>
                <span
                  className={cn(
                    'h-2 w-2 rounded-full',
                    isEpisodeSelectorCollapsed
                      ? 'animate-pulse bg-orange-400'
                      : 'bg-green-400',
                  )}
                  aria-hidden='true'
                />
              </button>

              {/* 实时下载速度: 速度明显变化时弹出, 3s 无更新自动隐藏 */}
              {speedBadge && !isVideoLoading && (
                <div className='pointer-events-none absolute right-2 top-2 z-40 rounded bg-black/45 px-2 py-0.5 text-xs text-white/85 backdrop-blur-sm'>
                  {speedBadge}
                </div>
              )}

              {/* 加载中的提示 (mpv 嵌入模式时画面由原生子窗口负责, 不盖遮罩) */}
              {isVideoLoading && !mpvState.active && (
                <div className='absolute inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm'>
                  <div className='flex flex-col items-center gap-3'>
                    <span className='text-white/80 text-sm'>
                      正在加载视频...
                    </span>
                  </div>
                </div>
              )}

              {/* mpv 嵌入模式控制条: 画面由原生子窗口渲染(占满上方),
                  此条恰好占据 mpv rect 扣除的底部预留高度 */}
              {mpvState.active && (
                <div className='absolute inset-x-0 bottom-0 z-30 flex h-12 items-center gap-2 bg-black/95 px-3 text-white'>
                  <button
                    type='button'
                    aria-label={mpvState.paused ? '播放' : '暂停'}
                    className='tap-target shrink-0 p-1 transition-opacity hover:opacity-80'
                    onClick={() => {
                      void invoke('mpv_embed_command', {
                        cmd: ['cycle', 'pause'],
                      }).catch(() => {});
                    }}
                  >
                    {mpvState.paused ? (
                      <Play className='h-5 w-5' />
                    ) : (
                      <Pause className='h-5 w-5' />
                    )}
                  </button>
                  <button
                    type='button'
                    aria-label='播放上一集'
                    className='tap-target shrink-0 p-1 transition-opacity hover:opacity-80'
                    onClick={() => handlePreviousEpisode()}
                  >
                    <SkipBack className='h-4 w-4' />
                  </button>
                  <span className='shrink-0 text-xs tabular-nums text-white/90'>
                    {formatTime(mpvState.time)} /{' '}
                    {formatTime(mpvState.duration)}
                  </span>
                  <input
                    type='range'
                    min={0}
                    max={mpvState.duration || 0}
                    step={0.1}
                    value={Math.min(mpvState.time, mpvState.duration || 0)}
                    aria-label='播放进度'
                    onChange={(e) => {
                      const t = Number(e.target.value);
                      setMpvState((s) => ({ ...s, time: t }));
                      void invoke('mpv_embed_command', {
                        cmd: ['seek', t, 'absolute'],
                      }).catch(() => {});
                    }}
                    className='h-1 min-w-0 flex-1 accent-emerald-500'
                  />
                  <button
                    type='button'
                    className='shrink-0 rounded px-1.5 py-0.5 text-xs tabular-nums text-white/90 ring-1 ring-white/25 transition-colors hover:bg-white/10'
                    title='播放速度'
                    onClick={() => {
                      const nextIdx = (mpvSpeedIdx + 1) % MPV_SPEEDS.length;
                      setMpvSpeedIdx(nextIdx);
                      void invoke('mpv_embed_command', {
                        cmd: ['set_property', 'speed', MPV_SPEEDS[nextIdx]],
                      }).catch(() => {});
                    }}
                  >
                    {MPV_SPEEDS[mpvSpeedIdx]}x
                  </button>
                  <input
                    type='range'
                    min={0}
                    max={100}
                    step={5}
                    value={mpvVolume}
                    title='音量'
                    aria-label='音量'
                    onChange={(e) => {
                      const v = Number(e.target.value);
                      setMpvVolume(v);
                      void invoke('mpv_embed_command', {
                        cmd: ['set_property', 'volume', v],
                      }).catch(() => {});
                    }}
                    className='hidden h-1 w-16 shrink-0 accent-emerald-500 sm:block'
                  />
                  <button
                    type='button'
                    aria-label='播放下一集'
                    className='tap-target shrink-0 p-1 transition-opacity hover:opacity-80'
                    onClick={() => handleNextEpisode()}
                  >
                    <SkipForward className='h-4 w-4' />
                  </button>
                  <button
                    type='button'
                    className='shrink-0 rounded px-2 py-0.5 text-xs text-white/80 ring-1 ring-white/25 transition-colors hover:bg-white/10'
                    onClick={() => {
                      void exitMpvMode(true);
                    }}
                  >
                    切回内置
                  </button>
                </div>
              )}

              {!swipeSeekOverlayPortalHost && swipeSeekOverlayNode}
            </div>
          </div>

          {/* 右侧栏（lg+）：小封面 + 元信息 + 描述 */}
          <aside className='hidden min-h-0 flex-col gap-4 lg:flex'>
            <div className='flex shrink-0 gap-4'>
              {/* 小封面 */}
              <div className='relative aspect-2/3 w-28 shrink-0 overflow-hidden rounded-xl bg-gray-300 dark:bg-gray-700'>
                {videoCover ? (
                  <img
                    src={proxiedCoverUrl}
                    alt={videoTitle}
                    className='h-full w-full object-cover'
                  />
                ) : (
                  <span className='absolute inset-0 flex items-center justify-center text-gray-600 dark:text-gray-400'>
                    封面图片
                  </span>
                )}
              </div>

              {/* 元信息 */}
              <div className='flex min-w-0 flex-col items-start gap-2 text-sm text-slate-700 dark:text-gray-300'>
                {detail?.class && (
                  <span className='font-semibold text-green-600 dark:text-green-400'>
                    {detail.class}
                  </span>
                )}
                {(detail?.year || videoYear) && (
                  <span className='text-gray-600 dark:text-gray-400'>
                    {detail?.year || videoYear}
                  </span>
                )}
                {detail?.type_name && (
                  <span className='text-gray-600 dark:text-gray-400'>
                    {detail.type_name}
                  </span>
                )}
                {detail?.source_name && (
                  <span className='rounded border border-gray-400 px-2 py-px text-gray-700 dark:border-gray-500 dark:text-gray-300'>
                    {detail.source_name}
                  </span>
                )}
              </div>
            </div>

            {/* 描述 */}
            {detail?.desc ? (
              <div className='min-h-0 flex-1 overflow-y-auto rounded-2xl bg-white/50 p-4 backdrop-blur-sm dark:bg-white/5'>
                <p className='whitespace-pre-line text-sm leading-relaxed text-slate-700 dark:text-gray-300'>
                  {detail.desc}
                </p>
              </div>
            ) : (
              <div className='flex min-h-0 flex-1 items-center justify-center rounded-2xl bg-white/50 p-4 backdrop-blur-sm dark:bg-white/5'>
                <span className='text-sm text-gray-400 dark:text-gray-500'>
                  暂无简介
                </span>
              </div>
            )}
          </aside>
        </div>

        {/* 站内源头列表 (播放器下方, 对齐 TVBox 线路切换; 单源头时隐藏) */}
        {detail?.play_groups && detail.play_groups.length > 1 && (
          <div className='flex flex-wrap items-center gap-2'>
            <span className='text-xs font-medium uppercase tracking-wide text-gray-500 dark:text-gray-400'>
              播放源
            </span>
            {detail.play_groups.map((group, index) => {
              const isCurrent = index === activeGroupIndex;
              return (
                <button
                  key={`${group.flag}-${index}`}
                  type='button'
                  onClick={() => handlePlayGroupChange(index)}
                  className={cn(
                    'cursor-pointer rounded-full px-3 py-1 text-xs font-medium transition-colors',
                    isCurrent
                      ? 'bg-green-500/15 text-green-600 ring-1 ring-green-500/40 dark:bg-green-500/20 dark:text-green-400'
                      : 'bg-gray-100 text-gray-600 ring-1 ring-gray-500/10 hover:bg-gray-200 dark:bg-white/5 dark:text-gray-400 dark:hover:bg-white/10',
                  )}
                >
                  {group.flag || `源 ${index + 1}`}
                </button>
              );
            })}
          </div>
        )}

        {/* 移动/平板：封面 + 描述（lg 隐藏） */}
        <section className='grid grid-cols-1 gap-4 lg:hidden'>
          <div className='rounded-2xl bg-white/50 p-4 backdrop-blur-sm dark:bg-white/5'>
            <div className='flex gap-4'>
              {/* 小封面 */}
              <div className='relative aspect-2/3 w-24 shrink-0 overflow-hidden rounded-xl bg-gray-300 dark:bg-gray-700'>
                {videoCover ? (
                  <img
                    src={proxiedCoverUrl}
                    alt={videoTitle}
                    className='h-full w-full object-cover'
                  />
                ) : (
                  <span className='absolute inset-0 flex items-center justify-center text-gray-600 dark:text-gray-400'>
                    封面图片
                  </span>
                )}
              </div>

              {/* 元信息 */}
              <div className='flex min-w-0 flex-col items-start gap-2 text-sm text-slate-700 dark:text-gray-300'>
                {detail?.class && (
                  <span className='font-semibold text-green-600 dark:text-green-400'>
                    {detail.class}
                  </span>
                )}
                {(detail?.year || videoYear) && (
                  <span className='text-gray-600 dark:text-gray-400'>
                    {detail?.year || videoYear}
                  </span>
                )}
                {detail?.type_name && (
                  <span className='text-gray-600 dark:text-gray-400'>
                    {detail.type_name}
                  </span>
                )}
                {detail?.source_name && (
                  <span className='rounded border border-gray-400 px-2 py-px text-gray-700 dark:border-gray-500 dark:text-gray-300'>
                    {detail.source_name}
                  </span>
                )}
              </div>
            </div>

            {/* 描述 */}
            {detail?.desc && (
              <p className='mt-4 whitespace-pre-line text-sm leading-relaxed text-slate-700 dark:text-gray-300'>
                {detail.desc}
              </p>
            )}
          </div>
        </section>

        {/* 选集面板（播放器下方） */}
        <div
          className={cn(
            'min-h-0 overflow-hidden transition-all duration-300 ease-in-out',
            'h-[28rem] max-[375px]:h-[24rem] sm:h-[26rem] min-[834px]:h-[30rem] lg:h-[26rem]',
            isEpisodeSelectorCollapsed && 'lg:hidden',
          )}
        >
          <EpisodeSelector
            totalEpisodes={totalEpisodes}
            episodes_titles={detail?.episodes_titles || []}
            value={currentEpisodeIndex + 1}
            onChange={handleEpisodeChange}
            videoTitle={searchTitle || videoTitle}
          />
        </div>
      </div>

      {swipeSeekOverlayPortalHost &&
        swipeSeekOverlayNode &&
        createPortal(swipeSeekOverlayNode, swipeSeekOverlayPortalHost)}

      {/* 跳过片头片尾设置面板 + Toast
          全屏时挂进全屏元素内部，否则会被 Plyr 的 z-index:10000000 盖住
          （原生全屏更彻底：top-layer 之外的节点根本不渲染） */}
      {(() => {
        const overlays = (
          <>
            <SkipConfigPanel
              isOpen={isSkipConfigPanelOpen}
              onClose={() => setIsSkipConfigPanelOpen(false)}
              config={skipConfig}
              onChange={handleSkipConfigChange}
              videoDuration={plyrRef.current?.duration || 0}
              currentTime={plyrRef.current?.currentTime || 0}
            />
            {toast.show && (
              <Toast
                message={toast.message}
                type={toast.type}
                duration={3000}
                onClose={() =>
                  setToast({ show: false, message: '', type: 'info' })
                }
              />
            )}
          </>
        );

        return modalPortalHost
          ? createPortal(overlays, modalPortalHost)
          : overlays;
      })()}
    </PageLayout>
  );
}

// FavoriteIcon 组件
const FavoriteIcon = ({ filled }: { filled: boolean }) => {
  if (filled) {
    return (
      <svg
        className='h-7 w-7'
        viewBox='0 0 24 24'
        xmlns='http://www.w3.org/2000/svg'
        aria-hidden='true'
      >
        <path
          d='M12 21.35l-1.45-1.32C5.4 15.36 2 12.28 2 8.5 2 5.42 4.42 3 7.5 3c1.74 0 3.41.81 4.5 2.09C13.09 3.81 14.76 3 16.5 3 19.58 3 22 5.42 22 8.5c0 3.78-3.4 6.86-8.55 11.54L12 21.35z'
          fill='#ef4444' /* Tailwind red-500 */
          stroke='#ef4444'
          strokeWidth='2'
          strokeLinecap='round'
          strokeLinejoin='round'
        />
      </svg>
    );
  }
  return (
    <Heart
      className='h-7 w-7 stroke-1 text-gray-600 dark:text-gray-300'
      aria-hidden='true'
    />
  );
};

export default function PlayPage() {
  return (
    <Suspense fallback={<div>Loading...</div>}>
      <PlayPageClient />
    </Suspense>
  );
}
