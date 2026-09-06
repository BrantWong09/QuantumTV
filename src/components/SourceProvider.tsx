'use client';

/* eslint-disable no-console */
import { invoke } from '@tauri-apps/api/core';
import {
  createContext,
  ReactNode,
  useCallback,
  useContext,
  useEffect,
  useState,
} from 'react';

import { ApiSite } from '@/lib/types';

const STORAGE_KEY = 'global.selectedSourceKey';
export const AUTO_SOURCE_KEY = 'auto';

interface SourceContextValue {
  /** 全部启用源 */
  sources: ApiSite[];
  /** 当前选中源 key，'auto' 表示豆瓣聚合 */
  currentSource: string;
  isLoadingSources: boolean;
  setCurrentSource: (key: string) => void;
  refreshSources: () => Promise<void>;
}

const SourceContext = createContext<SourceContextValue>({
  sources: [],
  currentSource: AUTO_SOURCE_KEY,
  isLoadingSources: false,
  setCurrentSource: () => {},
  refreshSources: async () => {},
});

export const useGlobalSource = () => useContext(SourceContext);

function SourceProviderClient({ children }: { children: ReactNode }) {
  const [sources, setSources] = useState<ApiSite[]>([]);
  const [currentSource, setCurrentSourceState] =
    useState<string>(AUTO_SOURCE_KEY);
  const [isLoadingSources, setIsLoadingSources] = useState(true);

  const fetchSources = useCallback(async () => {
    setIsLoadingSources(true);
    try {
      let useLocalSourceConfig =
        (process.env.NEXT_PUBLIC_STORAGE_TYPE || 'localstorage') ===
        'localstorage';
      try {
        const runtimeConfig = await invoke<{ use_local_source_config: boolean }>(
          'get_runtime_config',
        );
        useLocalSourceConfig = runtimeConfig.use_local_source_config;
      } catch {
        // 命令不可用时退回环境变量默认值
      }

      if (useLocalSourceConfig) {
        const config = await invoke<{
          SourceConfig?: Array<ApiSite & { disabled?: boolean }>;
        }>('get_config');
        const enabledSources = (config.SourceConfig || [])
          .filter((s) => !s.disabled)
          .map((s) => ({
            key: s.key,
            api: s.api,
            name: s.name,
            detail: s.detail,
            is_adult: s.is_adult,
            site_type: s.site_type,
            spider: s.spider,
            searchable: s.searchable,
          }));
        setSources(enabledSources);
        return;
      }

      const response = await fetch('/api/search/resources', {
        credentials: 'include',
      });
      if (!response.ok) {
        throw new Error('获取数据源列表失败');
      }
      const data: ApiSite[] = await response.json();
      setSources(data);
    } catch (err) {
      console.error('获取数据源列表失败', err);
    } finally {
      setIsLoadingSources(false);
    }
  }, []);

  useEffect(() => {
    void fetchSources();
  }, [fetchSources]);

  // 恢复持久化的选中源（需等 sources 加载完校验存在性）
  useEffect(() => {
    if (isLoadingSources) return;
    try {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved && (saved === AUTO_SOURCE_KEY || sources.some((s) => s.key === saved))) {
        setCurrentSourceState(saved);
      }
    } catch {
      /* ignore */
    }
  }, [isLoadingSources, sources]);

  const setCurrentSource = useCallback((key: string) => {
    setCurrentSourceState(key);
    try {
      localStorage.setItem(STORAGE_KEY, key);
    } catch {
      /* ignore */
    }
  }, []);

  return (
    <SourceContext.Provider
      value={{
        sources,
        currentSource,
        isLoadingSources,
        setCurrentSource,
        refreshSources: fetchSources,
      }}
    >
      {children}
    </SourceContext.Provider>
  );
}

export function SourceProvider({ children }: { children: ReactNode }) {
  return <SourceProviderClient>{children}</SourceProviderClient>;
}