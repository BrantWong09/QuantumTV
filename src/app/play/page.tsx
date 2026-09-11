/* eslint-disable react-hooks/exhaustive-deps, no-console, @next/next/no-img-element */

'use client';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Heart, Pause, Play, SkipBack, SkipForward } from 'lucide-react';
import { useSearchParams } from 'next/navigation';
import { Suspense, useEffect, useRef, useState } from 'react';

import {
  ApplySkipConfigResponse,
  InitializePlayerByQueryResponse,
  PlaybackState,
  PlayerInitialState,
  PlayerTickDecision,
  SearchResult,
} from '@/lib/types';
import { appLayoutClasses } from '@/lib/ui-layout';
import { cn, generateStorageKey, subscribeToDataUpdates } from '@/lib/utils';
import { useProxyImage } from '@/hooks/useProxyImage';

import EpisodeSelector from '@/components/EpisodeSelector';
import PageLayout from '@/components/PageLayout';
import SkipConfigPanel from '@/components/SkipConfigPanel';
import Toast from '@/components/Toast';

// -----------------------------------------------------------------------------
// V2 Phase 5 (docs/architecture/05-ipc.md): 本页只做 UI orchestration。
// 解析 (ResolverManager)、Gateway 包装、mpv 生命周期全部在 Rust 侧:
//   playback_play_episode(source, flag, episodeId) → loadfile
// 状态单一真相在 Rust PlaybackManager, 本页订阅 playback_* 事件渲染遥控面板。
// 画面由 mpv 独立窗口渲染 (方案 C), 页面内是控制面板。
// -----------------------------------------------------------------------------
const MPV_SPEEDS = [1, 1.25, 1.5, 2, 3, 0.75, 0.5];

function PlayPageClient() {
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

  // 视频基本信息
  const [videoTitle, setVideoTitle] = useState(searchParams.get('title') || '');
  const [videoYear, setVideoYear] = useState(searchParams.get('year') || '');
  const [videoCover, setVideoCover] = useState('');

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

  // Rust PlaybackManager 推送的播放状态 (单一真相, 本页不维护第二套)
  const [playback, setPlayback] = useState<PlaybackState>({
    status: 'idle',
    time: 0,
    duration: 0,
    resourceId: null,
    error: null,
  });
  const playbackRef = useRef(playback);
  useEffect(() => {
    playbackRef.current = playback;
  }, [playback]);
  const active =
    playback.status === 'loading' ||
    playback.status === 'playing' ||
    playback.status === 'paused' ||
    playback.status === 'ended';
  const activeRef = useRef(active);
  useEffect(() => {
    activeRef.current = active;
  }, [active]);

  // 优选开关（从 Rust 配置读取，默认 true，用于后端优选源）
  const [optimizationEnabled, setOptimizationEnabled] = useState<boolean>(true);

  // 去广告开关: m3u8 去广告在 Rust fetch_m3u8 侧处理, 直链/mpv 路径不受影响。
  // 该开关当前仅存配置 (原 Plyr 面板入口已随 HTML5 播放器删除)。
  const [blockAdEnabled] = useState<boolean>(true);
  void blockAdEnabled;

  // mpv 面板本地控制状态
  const [mpvSpeedIdx, setMpvSpeedIdx] = useState(0);
  const [mpvVolume, setMpvVolume] = useState(70);

  // 播放器壳 (页面全屏时作为弹窗挂载点)
  const playerShellRef = useRef<HTMLDivElement | null>(null);

  // 折叠状态（仅在 lg 及以上屏幕有效）
  const [isEpisodeSelectorCollapsed] = useState(false);

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

  // 页面全屏时面板挂进播放器壳, 避免被盖住
  // (Phase 5: 面板直接渲染在页面上, 挂载点保留给全屏扩展)
  void playerShellRef;

  // 播放进度保存相关
  const lastSaveTimeRef = useRef<number>(0);
  const lastStartedEpisodeRef = useRef<string>('');

  // mp4 直链加载失败后的重试状态: 网盘 dlink 签名有效期短(实测分钟级),
  // 播放报错时重新播放当前集换新链, 最多 1 次防止死循环
  const directRetryRef = useRef<{ episode: string; count: number }>({
    episode: '',
    count: 0,
  });

  // -----------------------------------------------------------------------------
  // 工具函数（Utils）

  // 当前集标题 (窗口标题/面板显示)
  const currentEpisodeTitle = () => {
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    return d?.episodes_titles?.[idx] || `${d?.title || '视频'} 第${idx + 1}集`;
  };

  // 当前集定位 key (source+episodeId), 用于去重与重试判断
  const currentEpisodeKey = () => {
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    return `${d?.source || ''}:${d?.episodes_raw?.[idx] || d?.episodes?.[idx] || idx}`;
  };

  // 播放当前集: 编排在 Rust (解析 → Gateway → loadfile), 前端只传"哪一集"
  const playCurrentEpisode = async (startAt?: number | null) => {
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    if (!d || !d.episodes || idx >= d.episodes.length) return;
    const episodeId = d.episodes_raw?.[idx] || d.episodes[idx];
    if (!episodeId) return;
    const group = d.play_groups?.[activeGroupIndex];
    const flag = group?.flag ?? '';
    setIsVideoLoading(true);
    try {
      await invoke('playback_play_episode', {
        source: d.source,
        flag,
        episodeId,
        title: d.title || null,
        episode: currentEpisodeTitle(),
        startAt: startAt ?? null,
      });
      lastStartedEpisodeRef.current = currentEpisodeKey();
    } catch (err) {
      const msg =
        (err as { toString?: () => string })?.toString?.() || '播放失败';
      console.error('[播放] playback_play_episode 失败:', err);
      showToast(msg, 'error');
      setIsVideoLoading(false);
    }
  };

  // 换集 → 播放 (Rust 编排)。startAt: 续播秒数
  const updatePlayback = async (
    detailData: SearchResult | null,
    episodeIndex: number,
    startAt: number | null,
  ) => {
    if (
      !detailData ||
      !detailData.episodes ||
      episodeIndex >= detailData.episodes.length
    ) {
      return;
    }
    const key = `${detailData.source}:${detailData.episodes_raw?.[episodeIndex] || detailData.episodes[episodeIndex] || episodeIndex}`;
    if (lastStartedEpisodeRef.current === key && activeRef.current) {
      return; // 同集不重复下发 (如 detail 重建触发)
    }
    directRetryRef.current = { episode: '', count: 0 };
    await playCurrentEpisode(startAt);
  };

  // 当集数索引变化时自动播放
  useEffect(() => {
    void updatePlayback(detail, currentEpisodeIndex, resumeTimeRef.current);
    resumeTimeRef.current = null;
  }, [detail, currentEpisodeIndex]);

  // 视频加载状态 (Loading 状态驱动)
  const [isVideoLoading, setIsVideoLoading] = useState(true);
  useEffect(() => {
    if (playback.status === 'playing' || playback.status === 'paused') {
      setIsVideoLoading(false);
    } else if (playback.status === 'loading') {
      setIsVideoLoading(true);
    }
  }, [playback.status]);

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
      // 立即更新 ref，确保事件处理器使用最新值
      skipConfigRef.current = newConfig;

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
    } catch (err) {
      console.error('[跳过配置] 更新配置失败:', err);
      showToast('更新跳过配置失败', 'error');
    }
  };

  // 页面全屏切换
  const togglePageFullscreen = () => {
    setIsPageFullscreen((prev) => !prev);
  };

  // ---------------------------------------------------------------------------
  // 集数切换
  // ---------------------------------------------------------------------------
  // 处理集数切换
  const handleEpisodeChange = (episodeNumber: number) => {
    if (episodeNumber >= 0 && episodeNumber < totalEpisodes) {
      // 在更换集数前保存当前播放进度
      if (activeRef.current) {
        void saveCurrentPlayProgress();
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
    if (activeRef.current && playbackRef.current.status === 'playing') {
      void saveCurrentPlayProgress();
    }

    // 计算默认组 (集数最多的一组), 切回时还原原始选集 (含首集直链化)
    const playGroups = current.play_groups ?? [];
    const maxCount = Math.max(...playGroups.map((g) => g.episodes.length));
    const defaultIdx = playGroups.findIndex(
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
    // 用选中组的选集重建 detail, 触发 updatePlayback 重播
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
      if (activeRef.current) {
        void saveCurrentPlayProgress();
      }
      setCurrentEpisodeIndex(idx - 1);
    }
  };

  const handleNextEpisode = () => {
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    if (d && d.episodes && idx < d.episodes.length - 1) {
      if (activeRef.current) {
        void saveCurrentPlayProgress();
      }
      setCurrentEpisodeIndex(idx + 1);
    }
  };

  // 保存播放进度 (时间取自 Rust 推送的播放状态)
  const saveCurrentPlayProgress = async () => {
    if (!currentSourceRef.current || !currentIdRef.current) {
      return;
    }
    if (!activeRef.current) {
      return;
    }

    const { time: currentTime, duration } = playbackRef.current;

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
    // 页面即将卸载时保存播放进度
    const handleBeforeUnload = () => {
      void saveCurrentPlayProgress();
      void invoke('playback_stop').catch(() => {});
    };

    // 页面可见性变化时保存播放进度
    const handleVisibilityChange = () => {
      if (document.visibilityState === 'hidden') {
        void saveCurrentPlayProgress();
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
  }, [currentEpisodeIndex, detail]);

  // ---------------------------------------------------------------------------
  // 播放状态订阅 (Rust 单一真相): playback_state 全量 + 旧事件兼容不依赖
  // ---------------------------------------------------------------------------
  useEffect(() => {
    let disposed = false;
    const unlisten = listen<PlaybackState>('playback_state', (e) => {
      if (disposed) return;
      setPlayback(e.payload);
    });
    // 初始化时拉一次快照 (事件丢失兜底)
    void invoke<PlaybackState>('playback_state')
      .then((s) => {
        if (!disposed && s.status !== 'idle') setPlayback(s);
      })
      .catch(() => {});
    return () => {
      disposed = true;
      void unlisten.then((f) => f());
    };
  }, []);

  // 标记"本页主动退出 mpv", 避免把 stop 误报为用户关窗
  const exitingRef = useRef(false);
  const stopPlayback = async () => {
    exitingRef.current = true;
    setPlayback({
      status: 'idle',
      time: 0,
      duration: 0,
      resourceId: null,
      error: null,
    });
    try {
      await invoke('playback_stop');
    } catch {
      /* mpv 可能已自行退出, 忽略 */
    }
    lastStartedEpisodeRef.current = '';
    setIsVideoLoading(false);
  };

  // mpv 意外退出 (用户没主动关): 提示; Rust 侧 crash recovery 自动重拉
  useEffect(() => {
    if (playback.status !== 'idle' || !activeRef.current) return;
    // active 态转 idle 且非本页主动 stop → 窗口被用户关闭
    // (本页主动退出会先置 exitingRef, 这里静默)
    if (exitingRef.current) {
      exitingRef.current = false;
      return;
    }
    showToast('mpv 播放器已关闭', 'info');
    setIsVideoLoading(false);
  }, [playback.status]);

  // 播放错误: 网盘 dlink 签名过期重试一次 (换新链)
  useEffect(() => {
    if (playback.status !== 'error') return;
    showToast(playback.error || '播放失败', 'error');
    setIsVideoLoading(false);
    const key = lastStartedEpisodeRef.current;
    const retry = directRetryRef.current;
    if (retry.episode === key || retry.count >= 1) {
      console.warn('[播放] 直链重试已用尽或同链重试, 放弃');
      return;
    }
    directRetryRef.current = { episode: key, count: retry.count + 1 };
    console.log('[播放] 播放失败, 重新编排换链…');
    const t = setTimeout(() => {
      void playCurrentEpisode(playback.time > 3 ? playback.time : null);
    }, 500);
    return () => clearTimeout(t);
  }, [playback.status, playback.error]);

  // 播完连播: ended → 下一集 / 提示最后一集
  useEffect(() => {
    if (playback.status !== 'ended') return;
    const d = detailRef.current;
    const idx = currentEpisodeIndexRef.current;
    if (d && d.episodes && idx < d.episodes.length - 1) {
      const t = setTimeout(() => setCurrentEpisodeIndex(idx + 1), 500);
      return () => clearTimeout(t);
    } else {
      showToast('已是最后一集', 'info');
    }
  }, [playback.status]);

  // 进度保存 + 跳过片头片尾: 复用 player_tick 的节流决策, 控制经 playback_* 下发
  useEffect(() => {
    if (!active || playback.status === 'paused') return;
    const timer = setInterval(async () => {
      const st = playbackRef.current;
      if (!activeRef.current || st.status === 'paused' || st.duration <= 0)
        return;
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
          void saveCurrentPlayProgress();
        }
        const skipAction = tickDecision.skipAction;
        if (
          skipAction &&
          typeof skipAction === 'object' &&
          'SkipIntro' in skipAction &&
          st.time > 0.5
        ) {
          const targetTime = skipAction.SkipIntro;
          await invoke('playback_seek', { secs: targetTime, absolute: true });
          showToast(`跳过片头，跳转到 ${formatTime(targetTime)}`, 'success');
        } else if (skipAction === 'SkipOutro' && st.time < st.duration - 1) {
          if (
            currentEpisodeIndexRef.current <
            (detailRef.current?.episodes?.length || 1) - 1
          ) {
            showToast('跳过片尾，跳转到下一集', 'info');
            setTimeout(() => handleNextEpisode(), 500);
          } else {
            await invoke('playback_set_paused', { paused: true });
            showToast('跳过片尾，但当前已是最后一集', 'info');
          }
        }
      } catch {
        /* player_tick 失败静默: 下一轮重试 */
      }
    }, 1000);
    return () => clearInterval(timer);
  }, [active, playback.status]);

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

  // ---------------------------------------------------------------------------
  // 键盘快捷键 (页面全屏 ESC + mpv 播放控制经 playback_* 下发)
  // ---------------------------------------------------------------------------
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

    if (!activeRef.current) return;
    if (e.key === ' ') {
      void invoke('playback_pause').catch(() => {});
      e.preventDefault();
    } else if (e.key === 'ArrowLeft' && !e.altKey) {
      void invoke('playback_seek', { secs: -10 }).catch(() => {});
      e.preventDefault();
    } else if (e.key === 'ArrowRight' && !e.altKey) {
      void invoke('playback_seek', { secs: 10 }).catch(() => {});
      e.preventDefault();
    } else if (e.key === 'ArrowUp') {
      void invoke('playback_add_volume', { delta: 5 }).catch(() => {});
      e.preventDefault();
    } else if (e.key === 'ArrowDown') {
      void invoke('playback_add_volume', { delta: -5 }).catch(() => {});
      e.preventDefault();
    } else if (e.key === 'f' || e.key === 'F') {
      togglePageFullscreen();
      e.preventDefault();
    }
  };

  useEffect(() => {
    document.addEventListener('keydown', handleKeyboardShortcuts);
    return () => {
      document.removeEventListener('keydown', handleKeyboardShortcuts);
    };
  }, []);

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

          // 设置播放器配置 (去广告在 Rust fetch_m3u8 侧, m3u8 播放路径生效)
          setOptimizationEnabled(initialState.optimization_enabled);

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
              </div>
            </div>

            {/* 错误信息 */}
            <h1 className='mb-3 text-2xl font-bold text-gray-900 dark:text-gray-100'>
              出错了
            </h1>
            <p className='mb-6 text-gray-600 dark:text-gray-400'>{error}</p>

            <button
              onClick={() => window.location.reload()}
              className='w-full px-6 py-3 bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-300 rounded-xl font-medium hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors duration-200'
            >
              🔄 重新尝试
            </button>
          </div>
        </div>
      </PageLayout>
    );
  }

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
              ref={playerShellRef}
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
              {/* 加载中的提示 (mpv 模式画面在独立窗口, 不盖遮罩) */}
              {isVideoLoading && !active && (
                <div className='absolute inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm'>
                  <div className='flex flex-col items-center gap-3'>
                    <span className='text-white/80 text-sm'>
                      正在加载视频...
                    </span>
                  </div>
                </div>
              )}

              {/* mpv 模式: 画面在独立 mpv 窗口, 这里是遥控面板 */}
              {active && (
                <div className='absolute inset-0 z-30 flex flex-col items-center justify-center gap-4 bg-black px-4 text-white'>
                  <div className='flex items-center gap-2 text-sm text-white/70'>
                    <span className='inline-block h-2 w-2 animate-pulse rounded-full bg-emerald-400' />
                    正在通过 mpv 播放 · {currentEpisodeTitle()}
                  </div>
                  <div className='text-2xl font-semibold tabular-nums'>
                    {formatTime(playback.time)} /{' '}
                    {formatTime(playback.duration)}
                  </div>
                  <div className='w-full max-w-md px-2'>
                    <input
                      type='range'
                      min={0}
                      max={playback.duration || 0}
                      step={0.1}
                      value={Math.min(playback.time, playback.duration || 0)}
                      aria-label='播放进度'
                      onChange={(e) => {
                        const t = Number(e.target.value);
                        setPlayback((s) => ({ ...s, time: t }));
                        void invoke('playback_seek', {
                          secs: t,
                          absolute: true,
                        }).catch(() => {});
                      }}
                      className='h-1.5 w-full accent-emerald-500'
                    />
                  </div>
                  <div className='flex items-center gap-4'>
                    <button
                      type='button'
                      aria-label='播放上一集'
                      className='tap-target p-2 transition-opacity hover:opacity-80'
                      onClick={() => handlePreviousEpisode()}
                    >
                      <SkipBack className='h-5 w-5' />
                    </button>
                    <button
                      type='button'
                      aria-label={
                        playback.status === 'paused' ? '播放' : '暂停'
                      }
                      className='flex h-12 w-12 items-center justify-center rounded-full bg-white/10 ring-1 ring-white/25 transition-colors hover:bg-white/20'
                      onClick={() => {
                        void invoke('playback_pause').catch(() => {});
                      }}
                    >
                      {playback.status === 'paused' ? (
                        <Play className='h-6 w-6' />
                      ) : (
                        <Pause className='h-6 w-6' />
                      )}
                    </button>
                    <button
                      type='button'
                      aria-label='播放下一集'
                      className='tap-target p-2 transition-opacity hover:opacity-80'
                      onClick={() => handleNextEpisode()}
                    >
                      <SkipForward className='h-5 w-5' />
                    </button>
                  </div>
                  <div className='flex items-center gap-3 text-sm'>
                    <button
                      type='button'
                      className='rounded px-2 py-1 tabular-nums ring-1 ring-white/25 transition-colors hover:bg-white/10'
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
                      className='h-1 w-24 accent-emerald-500'
                    />
                    <button
                      type='button'
                      className='rounded px-2 py-1 text-white/80 ring-1 ring-white/25 transition-colors hover:bg-white/10'
                      onClick={() => {
                        void stopPlayback();
                      }}
                    >
                      停止播放
                    </button>
                  </div>
                  <p className='max-w-sm text-center text-xs text-white/50'>
                    mpv 播放窗口已置顶打开, 可拖动/全屏; 关闭该窗口即停止播放
                  </p>
                </div>
              )}

              {/* 空闲态: 提示选择剧集开始播放 */}
              {!active && !isVideoLoading && (
                <div className='absolute inset-0 z-20 flex flex-col items-center justify-center gap-3 text-white/60'>
                  <Play className='h-10 w-10' aria-hidden='true' />
                  <span className='text-sm'>选择剧集开始播放</span>
                </div>
              )}
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

      {/* 跳过片头片尾设置面板 + Toast */}
      <SkipConfigPanel
        isOpen={isSkipConfigPanelOpen}
        onClose={() => setIsSkipConfigPanelOpen(false)}
        config={skipConfig}
        onChange={handleSkipConfigChange}
        videoDuration={playback.duration}
        currentTime={playback.time}
      />
      {toast.show && (
        <Toast
          message={toast.message}
          type={toast.type}
          duration={3000}
          onClose={() => setToast({ show: false, message: '', type: 'info' })}
        />
      )}
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
