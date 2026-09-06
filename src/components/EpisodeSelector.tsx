import { useRouter } from 'next/navigation';
import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';

interface EpisodeSelectorProps {
  /** 总集数 */
  totalEpisodes: number;
  /** 剧集标题 */
  episodes_titles: string[];
  /** 每页显示多少集，默认 50 */
  episodesPerPage?: number;
  /** 当前选中的集数（1 开始） */
  value?: number;
  /** 用户点击选集后的回调 */
  onChange?: (episodeNumber: number) => void;
  /** 用于"影片匹配有误？点击去搜索"的搜索词 */
  videoTitle?: string;
}

/**
 * 选集组件，支持分页、自动滚动聚焦当前分页标签。
 */
const EpisodeSelector: React.FC<EpisodeSelectorProps> = ({
  totalEpisodes,
  episodes_titles,
  episodesPerPage = 50,
  value = 1,
  onChange,
  videoTitle,
}) => {
  const router = useRouter();
  const pageCount = Math.ceil(totalEpisodes / episodesPerPage);

  // 当前分页索引（0 开始）
  const initialPage = Math.floor((value - 1) / episodesPerPage);
  const [currentPage, setCurrentPage] = useState<number>(initialPage);

  // 是否倒序显示
  const [descending, setDescending] = useState<boolean>(false);

  // 根据 descending 状态计算实际显示的分页索引
  const displayPage = useMemo(() => {
    if (descending) {
      return pageCount - 1 - currentPage;
    }
    return currentPage;
  }, [currentPage, descending, pageCount]);

  // 升序分页标签
  const categoriesAsc = useMemo(() => {
    return Array.from({ length: pageCount }, (_, i) => {
      const start = i * episodesPerPage + 1;
      const end = Math.min(start + episodesPerPage - 1, totalEpisodes);
      return { start, end };
    });
  }, [pageCount, episodesPerPage, totalEpisodes]);

  // 根据 descending 状态决定分页标签的排序和内容
  const categories = useMemo(() => {
    if (descending) {
      // 倒序时，label 也倒序显示
      return [...categoriesAsc]
        .reverse()
        .map(({ start, end }) => `${end}-${start}`);
    }
    return categoriesAsc.map(({ start, end }) => `${start}-${end}`);
  }, [categoriesAsc, descending]);

  const categoryContainerRef = useRef<HTMLDivElement>(null);
  const buttonRefs = useRef<(HTMLButtonElement | null)[]>([]);

  // 添加鼠标悬停状态管理
  const [isCategoryHovered, setIsCategoryHovered] = useState(false);

  // 阻止页面竖向滚动
  const preventPageScroll = useCallback(
    (e: WheelEvent) => {
      if (isCategoryHovered) {
        e.preventDefault();
      }
    },
    [isCategoryHovered],
  );

  // 处理滚轮事件，实现横向滚动
  const handleWheel = useCallback(
    (e: WheelEvent) => {
      if (isCategoryHovered && categoryContainerRef.current) {
        e.preventDefault(); // 阻止默认的竖向滚动

        const container = categoryContainerRef.current;
        const scrollAmount = e.deltaY * 2; // 调整滚动速度

        // 根据滚轮方向进行横向滚动
        container.scrollBy({
          left: scrollAmount,
          behavior: 'smooth',
        });
      }
    },
    [isCategoryHovered],
  );

  // 添加全局wheel事件监听器
  useEffect(() => {
    if (isCategoryHovered) {
      // 鼠标悬停时阻止页面滚动
      document.addEventListener('wheel', preventPageScroll, { passive: false });
      document.addEventListener('wheel', handleWheel, { passive: false });
    } else {
      // 鼠标离开时恢复页面滚动
      document.removeEventListener('wheel', preventPageScroll);
      document.removeEventListener('wheel', handleWheel);
    }

    return () => {
      document.removeEventListener('wheel', preventPageScroll);
      document.removeEventListener('wheel', handleWheel);
    };
  }, [isCategoryHovered, preventPageScroll, handleWheel]);

  // 当分页切换时，将激活的分页标签滚动到视口中间
  useEffect(() => {
    const btn = buttonRefs.current[displayPage];
    const container = categoryContainerRef.current;
    if (btn && container) {
      // 手动计算滚动位置，只滚动分页标签容器
      const containerRect = container.getBoundingClientRect();
      const btnRect = btn.getBoundingClientRect();
      const scrollLeft = container.scrollLeft;

      // 计算按钮相对于容器的位置
      const btnLeft = btnRect.left - containerRect.left + scrollLeft;
      const btnWidth = btnRect.width;
      const containerWidth = containerRect.width;

      // 计算目标滚动位置，使按钮居中
      const targetScrollLeft = btnLeft - (containerWidth - btnWidth) / 2;

      // 平滑滚动到目标位置
      container.scrollTo({
        left: targetScrollLeft,
        behavior: 'smooth',
      });
    }
  }, [displayPage, pageCount]);

  const handleCategoryClick = useCallback(
    (index: number) => {
      if (descending) {
        // 在倒序时，需要将显示索引转换为实际索引
        setCurrentPage(pageCount - 1 - index);
      } else {
        setCurrentPage(index);
      }
    },
    [descending, pageCount],
  );

  const handleEpisodeClick = useCallback(
    (episodeNumber: number) => {
      onChange?.(episodeNumber);
    },
    [onChange],
  );

  const currentStart = currentPage * episodesPerPage + 1;
  const currentEnd = Math.min(
    currentStart + episodesPerPage - 1,
    totalEpisodes,
  );

  return (
    <div className='flex h-full min-h-0 flex-col overflow-hidden rounded-[1.4rem] border border-black/5 bg-black/10 px-3.5 py-0 shadow-[0_16px_40px_-28px_rgba(15,23,42,0.45)] backdrop-blur-sm sm:px-4 min-[834px]:rounded-[1.55rem] min-[834px]:px-5 dark:border-white/15 dark:bg-white/5'>
      {/* 分类标签 */}
      <div className='-mx-3 mb-4 flex shrink-0 items-center gap-4 border-b border-gray-300 px-3 sm:-mx-4 sm:px-4 min-[834px]:-mx-5 min-[834px]:px-5 dark:border-gray-700'>
        <div
          className='flex-1 overflow-x-auto'
          ref={categoryContainerRef}
          onMouseEnter={() => setIsCategoryHovered(true)}
          onMouseLeave={() => setIsCategoryHovered(false)}
        >
          <div className='flex gap-2 min-w-max'>
            {categories.map((label, idx) => {
              const isActive = idx === displayPage;
              return (
                <button
                  key={label}
                  ref={(el) => {
                    buttonRefs.current[idx] = el;
                  }}
                  onClick={() => handleCategoryClick(idx)}
                  className={`w-20 relative py-2 text-sm font-medium transition-colors whitespace-nowrap shrink-0 text-center 
                  ${
                    isActive
                      ? 'text-green-500 dark:text-green-400'
                      : 'text-gray-700 hover:text-green-600 dark:text-gray-300 dark:hover:text-green-400'
                  }
                `.trim()}
                >
                  {label}
                  {isActive && (
                    <div className='absolute bottom-0 left-0 right-0 h-0.5 bg-green-500 dark:bg-green-400' />
                  )}
                </button>
              );
            })}
          </div>
        </div>
        {/* 向上/向下按钮 */}
        <button
          className='shrink-0 w-8 h-8 rounded-md flex items-center justify-center text-gray-700 hover:text-green-600 hover:bg-gray-100 dark:text-gray-300 dark:hover:text-green-400 dark:hover:bg-white/20 transition-colors transform -translate-y-1'
          onClick={() => {
            // 切换集数排序（正序/倒序）
            setDescending((prev) => !prev);
          }}
        >
          <svg
            className='w-4 h-4'
            fill='none'
            stroke='currentColor'
            viewBox='0 0 24 24'
          >
            <path
              strokeLinecap='round'
              strokeLinejoin='round'
              strokeWidth='2'
              d='M7 16V4m0 0L3 8m4-4l4 4m6 0v12m0 0l4-4m-4 4l-4-4'
            />
          </svg>
        </button>
      </div>

      {/* 集数网格 */}
      <div className='flex flex-wrap gap-3 overflow-y-auto flex-1 content-start pb-4'>
        {(() => {
          const len = currentEnd - currentStart + 1;
          const episodes = Array.from({ length: len }, (_, i) =>
            descending ? currentEnd - i : currentStart + i,
          );
          return episodes;
        })().map((episodeNumber) => {
          const isActive = episodeNumber === value;
          return (
            <button
              key={episodeNumber}
              onClick={() => handleEpisodeClick(episodeNumber - 1)}
              className={`h-10 min-w-10 px-3 py-2 flex items-center justify-center text-sm font-medium rounded-md transition-all duration-200 whitespace-nowrap font-mono
                ${
                  isActive
                    ? 'bg-green-500 text-white shadow-lg shadow-green-500/25 dark:bg-green-600'
                    : 'bg-gray-200 text-gray-700 hover:bg-gray-300 hover:scale-105 dark:bg-white/10 dark:text-gray-300 dark:hover:bg-white/20'
                }`.trim()}
            >
              {(() => {
                const title = episodes_titles?.[episodeNumber - 1];
                if (!title) {
                  return episodeNumber;
                }
                // 如果匹配"第X集"、"第X话"、"X集"、"X话"格式，提取中间的数字
                const match = title.match(/(?:第)?(\d+)(?:集|话)/);
                if (match) {
                  return match[1];
                }
                return title;
              })()}
            </button>
          );
        })}
      </div>

      {/* 去搜索提示 */}
      <div className='shrink-0 mt-auto pt-2 border-t border-gray-400 dark:border-gray-700'>
        <button
          onClick={() => {
            if (videoTitle) {
              router.push(`/search?q=${encodeURIComponent(videoTitle)}`);
            }
          }}
          className='w-full text-center text-xs text-gray-500 dark:text-gray-400 hover:text-green-500 dark:hover:text-green-400 transition-colors py-2'
        >
          影片匹配有误？点击去搜索
        </button>
      </div>
    </div>
  );
};

export default EpisodeSelector;