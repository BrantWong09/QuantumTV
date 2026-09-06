'use client';

import { ChevronDown, Clapperboard } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';

import { AUTO_SOURCE_KEY, useGlobalSource } from './SourceProvider';

function SourceSwitcher() {
  const { sources, currentSource, setCurrentSource, isLoadingSources } =
    useGlobalSource();
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  const currentName =
    currentSource === AUTO_SOURCE_KEY
      ? '豆瓣推荐'
      : (sources.find((s) => s.key === currentSource)?.name ??
        '豆瓣推荐');

  // 点击外部关闭
  useEffect(() => {
    if (!open) return;
    const handleClickOutside = (e: MouseEvent) => {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    };
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', handleClickOutside);
    document.addEventListener('keydown', handleEscape);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
      document.removeEventListener('keydown', handleEscape);
    };
  }, [open]);

  const handleSelect = (key: string) => {
    setCurrentSource(key);
    setOpen(false);
  };

  return (
    <div ref={containerRef} className='relative'>
      <button
        type='button'
        onClick={() => setOpen((prev) => !prev)}
        aria-haspopup='listbox'
        aria-expanded={open}
        title='切换内容源'
        className='tap-target flex max-w-[13rem] shrink-0 cursor-pointer items-center gap-1.5 rounded-full px-2.5 py-1.5 text-sm font-medium text-gray-700 transition-colors hover:bg-black/5 dark:text-gray-200 dark:hover:bg-white/10'
      >
        <Clapperboard className='h-4 w-4 shrink-0 text-purple-500 dark:text-purple-400' />
        <span className='truncate'>{currentName}</span>
        <ChevronDown
          className={`h-3.5 w-3.5 shrink-0 transition-transform ${open ? 'rotate-180' : ''}`}
        />
      </button>

      {open && (
        <div
          role='listbox'
          className='absolute left-0 top-full z-[90] mt-2 max-h-[60vh] w-64 min-w-max max-w-[80vw] overflow-y-auto rounded-2xl border border-white/30 bg-white/95 p-1.5 shadow-xl shadow-purple-500/10 backdrop-blur-2xl dark:border-white/10 dark:bg-gray-900/95'
        >
          <button
            type='button'
            role='option'
            aria-selected={currentSource === AUTO_SOURCE_KEY}
            onClick={() => handleSelect(AUTO_SOURCE_KEY)}
            className={`flex w-full cursor-pointer items-center gap-2 rounded-xl px-3 py-2 text-left text-sm transition-colors ${
              currentSource === AUTO_SOURCE_KEY
                ? 'bg-purple-500/15 font-semibold text-purple-700 dark:text-purple-300'
                : 'text-gray-700 hover:bg-black/5 dark:text-gray-200 dark:hover:bg-white/10'
            }`}
          >
            豆瓣推荐
          </button>
          {isLoadingSources && sources.length === 0 && (
            <div className='px-3 py-2 text-sm text-gray-500 dark:text-gray-400'>
              加载中...
            </div>
          )}
          {sources.map((s) => (
            <button
              key={s.key}
              type='button'
              role='option'
              aria-selected={s.key === currentSource}
              onClick={() => handleSelect(s.key)}
              className={`flex w-full cursor-pointer items-center gap-2 rounded-xl px-3 py-2 text-left text-sm transition-colors ${
                s.key === currentSource
                  ? 'bg-purple-500/15 font-semibold text-purple-700 dark:text-purple-300'
                  : 'text-gray-700 hover:bg-black/5 dark:text-gray-200 dark:hover:bg-white/10'
              }`}
            >
              <span className='truncate'>{s.name}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export default SourceSwitcher;