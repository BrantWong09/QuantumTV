'use client';

import { invoke } from '@tauri-apps/api/core';
import {
  Cat,
  Clover,
  Film,
  Home,
  LucideIcon,
  Search,
  Sparkles,
  Tv,
} from 'lucide-react';
import { usePathname, useSearchParams } from 'next/navigation';
import {
  memo,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from 'react';

import FastLink from './FastLink';
import { useSite } from './SiteProvider';
import { AUTO_SOURCE_KEY, useGlobalSource } from './SourceProvider';
import SourceSwitcher from './SourceSwitcher';
import { ThemeToggle } from './ThemeToggle';
import { UserMenu } from './UserMenu';

interface SourceCategoryItem {
  type_id: string | number;
  type_name: string;
  type_pid?: string | number;
}

// 豆瓣 4 类槽位与源分类名关键词映射
const DOUBAN_SLOTS: {
  label: string;
  type: string;
  keywords: string[];
  icon: LucideIcon;
  chip: string;
}[] = [
  {
    label: '电影',
    type: 'movie',
    keywords: ['电影', '影片'],
    icon: Film,
    chip: 'chip-movie',
  },
  {
    label: '剧集',
    type: 'tv',
    keywords: ['电视剧', '连续剧', '剧集', '电视', '剧场'],
    icon: Tv,
    chip: 'chip-tv',
  },
  {
    label: '动漫',
    type: 'anime',
    keywords: ['动漫', '动画', '番剧', '日漫', '国漫'],
    icon: Cat,
    chip: 'chip-anime',
  },
  {
    label: '综艺',
    type: 'show',
    keywords: ['综艺', '娱乐', '真人秀'],
    icon: Clover,
    chip: 'chip-show',
  },
];

interface NavItemDef {
  href: string;
  icon: LucideIcon;
  label: string;
  chip: string;
  kind: 'exact' | 'douban-auto' | 'douban-source';
  doubanType?: string;
  typeId?: string;
  sourceKey?: string;
}

function NavItems() {
  const pathname = usePathname();
  const searchParams = useSearchParams();
  const { currentSource } = useGlobalSource();
  const [hydrated, setHydrated] = useState(false);
  const [sourceCategories, setSourceCategories] = useState<
    SourceCategoryItem[]
  >([]);

  useEffect(() => {
    setHydrated(true);
  }, []);

  // 选定具体源时拉取该源分类，用于顶栏 4 个入口
  useEffect(() => {
    if (currentSource === AUTO_SOURCE_KEY) {
      setSourceCategories([]);
      return;
    }
    let cancelled = false;
    invoke<SourceCategoryItem[]>('get_source_categories', {
      sourceKey: currentSource,
    })
      .then((cats) => {
        if (!cancelled) setSourceCategories(cats || []);
      })
      .catch(() => {
        if (!cancelled) setSourceCategories([]);
      });
    return () => {
      cancelled = true;
    };
  }, [currentSource]);

  const navItems = useMemo<NavItemDef[]>(() => {
    const items: NavItemDef[] = [
      {
        href: '/',
        icon: Home,
        label: '首页',
        chip: 'chip-home',
        kind: 'exact',
      },
      {
        href: '/search',
        icon: Search,
        label: '搜索',
        chip: 'chip-search',
        kind: 'exact',
      },
    ];

    if (currentSource === AUTO_SOURCE_KEY) {
      DOUBAN_SLOTS.forEach((slot) => {
        items.push({
          href: `/douban?type=${slot.type}`,
          icon: slot.icon,
          label: slot.label,
          chip: slot.chip,
          kind: 'douban-auto',
          doubanType: slot.type,
        });
      });
      return items;
    }

    // 具体源：4 个槽位按语义映射到该源分类，匹配不到的隐藏
    DOUBAN_SLOTS.forEach((slot) => {
      const matched = sourceCategories.find((cat) =>
        slot.keywords.some((kw) => cat.type_name?.includes(kw)),
      );
      if (!matched) return;
      const typeId = String(matched.type_id);
      items.push({
        href: `/douban?source=${encodeURIComponent(currentSource)}&type=${slot.type}&type_id=${encodeURIComponent(typeId)}`,
        icon: slot.icon,
        label: matched.type_name || slot.label,
        chip: slot.chip,
        kind: 'douban-source',
        doubanType: slot.type,
        typeId,
        sourceKey: currentSource,
      });
    });

    return items;
  }, [currentSource, sourceCategories]);

  const normalizePath = useCallback((input: string | null | undefined) => {
    if (!input) return '/';
    const pathOnly = input.split('?')[0] || '/';
    if (pathOnly === '/') return '/';
    return pathOnly.endsWith('/') ? pathOnly.slice(0, -1) || '/' : pathOnly;
  }, []);

  const normalizedPathname = useMemo(
    () => normalizePath(pathname),
    [pathname, normalizePath],
  );

  const currentType = useMemo(
    () => (searchParams.get('type') || '').toLowerCase(),
    [searchParams],
  );

  const currentTypeId = useMemo(
    () => searchParams.get('type_id') || '',
    [searchParams],
  );

  const currentSourceParam = useMemo(
    () => searchParams.get('source') || '',
    [searchParams],
  );

  const isActive = useCallback(
    (item: NavItemDef) => {
      if (item.kind === 'exact') {
        return normalizedPathname === normalizePath(item.href);
      }
      // douban 类入口
      if (normalizedPathname !== '/douban') return false;
      if (currentType !== (item.doubanType || '').toLowerCase()) return false;
      if (item.kind === 'douban-source') {
        return currentSourceParam === item.sourceKey && currentTypeId === item.typeId;
      }
      return !currentTypeId;
    },
    [normalizedPathname, normalizePath, currentType, currentTypeId, currentSourceParam],
  );

  return (
    <>
      {navItems.map((item) => {
        const Icon = item.icon;
        const active = hydrated && isActive(item);

        return (
          <FastLink
            key={`${item.kind}-${item.label}-${item.href}`}
            href={item.href}
            useTransitionNav
            className={`tap-target inline-flex shrink-0 cursor-pointer items-center gap-2 rounded-full px-3 py-2 text-sm font-medium min-[1440px]:text-[0.95rem]
              transition-colors duration-200
              glass-chip chip-glow chip-theme ${item.chip}
              ${
                active
                  ? 'ring-2 ring-purple-500/40 dark:ring-purple-400/40 shadow-md shadow-purple-500/20'
                  : 'hover:shadow-sm'
              }`}
          >
            <Icon
              className={`h-4 w-4 ${active ? 'text-purple-600 dark:text-purple-300' : ''}`}
            />
            <span>{item.label}</span>
          </FastLink>
        );
      })}
    </>
  );
}

function TopNavbar() {
  const { siteName } = useSite();

  return (
    <header
      className='fixed left-0 right-0 top-0 z-[80] hidden lg:block'
      style={{ contain: 'layout paint' }}
    >
      <div className='mx-auto w-full max-w-[1720px] px-3 min-[834px]:px-7 lg:px-8 min-[1440px]:px-10'>
        <div className='relative mt-3 overflow-hidden rounded-2xl min-[1440px]:mt-4'>
          <div className='absolute inset-0 rounded-2xl bg-white/75 backdrop-blur-2xl dark:bg-gray-950/70' />
          <div className='absolute inset-0 rounded-2xl border border-white/30 shadow-xl shadow-purple-500/10 dark:border-white/10' />
          <div className='absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-purple-500/40 to-transparent' />

          <nav className='relative grid h-16 grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-3 px-4 min-[1440px]:h-[4.25rem]'>
            <div className='flex min-w-0 justify-self-start items-center gap-2'>
              <FastLink
                href='/'
                useTransitionNav
                className='flex min-w-0 select-none items-center gap-2'
              >
                <Sparkles className='h-6 w-6 text-purple-500 dark:text-purple-400' />
                <span className='truncate text-lg font-black tracking-tight deco-brand xl:text-xl'>
                  {siteName || 'QuantumTV'}
                </span>
              </FastLink>
              <Suspense fallback={null}>
                <SourceSwitcher />
              </Suspense>
            </div>

            <div className='justify-self-center min-w-0'>
              <div className='mx-auto flex max-w-[54vw] items-center justify-center gap-1 overflow-x-auto py-1 scrollbar-hide'>
                <Suspense fallback={null}>
                  <NavItems />
                </Suspense>
              </div>
            </div>

            <div className='flex min-w-0 justify-self-end items-center gap-2'>
              <ThemeToggle />
              <UserMenu />
            </div>
          </nav>
        </div>
      </div>
    </header>
  );
}

export default memo(TopNavbar);