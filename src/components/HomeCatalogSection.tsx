/* eslint-disable no-console */
'use client';

import { invoke } from '@tauri-apps/api/core';
import { useRouter } from 'next/navigation';
import { useEffect, useRef, useState } from 'react';

import { HomeCatalogResponse } from '@/lib/types';
import { appLayoutClasses, getRailItemClass } from '@/lib/ui-layout';
import { useSourceFilter } from '@/hooks/useSourceFilter';

import ScrollableRow from './ScrollableRow';
import VideoCard from './VideoCard';

const STORAGE_KEY = 'home.selectedSourceKey';
const EMPTY_CATALOG: HomeCatalogResponse = {
  source_key: '',
  source_name: '',
  site_type: 1,
  categories: [],
};

function HomeSourceSwitcher({
  sources,
  selected,
  onSelect,
}: {
  sources: { key: string; name: string }[];
  selected: string;
  onSelect: (key: string) => void;
}) {
  return (
    <div className='mb-6 flex justify-center'>
      <div className='mx-auto flex max-w-full flex-wrap items-center justify-center gap-1.5 overflow-x-auto py-1 scrollbar-hide'>
        {sources.map((s) => {
          const active = s.key === selected;
          return (
            <button
              key={s.key}
              type='button'
              onClick={() => onSelect(s.key)}
              className={`tap-target inline-flex shrink-0 cursor-pointer items-center gap-1.5 rounded-full px-3 py-1.5 text-sm font-medium transition-colors duration-200
                ${
                  active
                    ? 'bg-purple-600 text-white shadow-md shadow-purple-500/30 dark:bg-purple-500'
                    : 'glass-chip chip-theme chip-glow chip-home hover:shadow-sm'
                }`}
            >
              {s.name}
            </button>
          );
        })}
      </div>
    </div>
  );
}

export default function HomeCatalogSection() {
  const router = useRouter();
  const { sources, isLoadingSources } = useSourceFilter();

  const [selectedSource, setSelectedSource] = useState<string>('');
  const [catalog, setCatalog] = useState<HomeCatalogResponse | null>(null);
  const [loading, setLoading] = useState(true);

  // 组件内缓存：Map<source_key, response>
  const cacheRef = useRef(new Map<string, HomeCatalogResponse>());

  // 初始化选中源（默认第一个）
  useEffect(() => {
    if (sources.length === 0) return;
    let initial = sources[0].key;
    try {
      const saved = localStorage.getItem(STORAGE_KEY);
      if (saved && sources.some((s) => s.key === saved)) {
        initial = saved;
      }
    } catch {
      /* ignore */
    }
    setSelectedSource(initial);
  }, [sources]);

  // 拉取 / 复用缓存
  useEffect(() => {
    if (!selectedSource) return;
    let cancelled = false;

    const cached = cacheRef.current.get(selectedSource);
    if (cached) {
      setCatalog(cached);
      setLoading(false);
      // 无目录 → 静默跳转搜索页
      if (cached.categories.length === 0) {
        router.push('/search');
      }
      return;
    }

    setLoading(true);
    invoke<HomeCatalogResponse>('get_home_catalog', {
      sourceKey: selectedSource,
    })
      .then((resp) => {
        if (cancelled) return;
        cacheRef.current.set(selectedSource, resp);
        setCatalog(resp);
        if (resp.categories.length === 0) {
          router.push('/search');
        }
      })
      .catch((err) => {
        console.error('获取首页目录失败:', err);
        if (cancelled) return;
        setCatalog(EMPTY_CATALOG);
        router.push('/search');
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [selectedSource, router]);

  const handleSelect = (key: string) => {
    setSelectedSource(key);
    try {
      localStorage.setItem(STORAGE_KEY, key);
    } catch {
      /* ignore */
    }
  };

  // 骨架屏
  const SkeletonRow = () => (
    <section className={appLayoutClasses.sectionGap}>
      <div className='mb-5 h-5 w-24 bg-gray-200 dark:bg-gray-700 rounded animate-pulse' />
      <div className='flex gap-3 overflow-x-auto pb-2 scrollbar-hide'>
        {Array.from({ length: 6 }).map((_, i) => (
          <div
            key={i}
            className={`${getRailItemClass('default')} aspect-2/3 w-full max-w-[10.75rem] rounded-xl bg-gradient-to-br from-gray-200 to-gray-300 dark:from-gray-800 dark:to-gray-700 animate-pulse`}
          />
        ))}
      </div>
    </section>
  );

  if (isLoadingSources || sources.length === 0) {
    return null;
  }

  if (loading || !catalog) {
    return (
      <>
        <HomeSourceSwitcher
          sources={sources}
          selected={selectedSource || sources[0]?.key || ''}
          onSelect={handleSelect}
        />
        <SkeletonRow />
      </>
    );
  }

  // 目录为空时正在跳转，仅渲染切换器
  if (catalog.categories.length === 0) {
    return (
      <HomeSourceSwitcher
        sources={sources}
        selected={catalog.source_key || selectedSource}
        onSelect={handleSelect}
      />
    );
  }

  return (
    <>
      <HomeSourceSwitcher
        sources={sources}
        selected={catalog.source_key}
        onSelect={handleSelect}
      />
      {catalog.categories.map((cat) => (
        <section key={cat.type_id} className={appLayoutClasses.sectionGap}>
          <div className='mb-5 flex items-center justify-between'>
            <h2 className='text-lg font-bold text-gray-800 max-[375px]:text-base min-[834px]:text-[1.35rem] min-[1440px]:text-[1.5rem] dark:text-gray-100'>
              {cat.type_name}
            </h2>
          </div>
          <ScrollableRow>
            {cat.list.map((card) => (
              <div key={card.id} className={getRailItemClass('default')}>
                <VideoCard
                  from='home'
                  id={card.id}
                  source={card.source}
                  title={card.title}
                  poster={card.poster}
                  episodes={card.episodes.length || undefined}
                  source_name={card.source_name}
                  year={card.year || undefined}
                />
              </div>
            ))}
          </ScrollableRow>
        </section>
      ))}
    </>
  );
}