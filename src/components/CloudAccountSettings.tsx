'use client';

import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

const DRIVE_META: Record<
  string,
  { name: string; desc: string }
> = {
  quark: { name: '夸克网盘', desc: '需在模拟器中登录夸克网盘账号后使用夸克线路' },
  uc: { name: 'UC 网盘', desc: '需在模拟器中登录 UC 网盘账号后使用 UC 线路' },
  baidu: { name: '百度网盘', desc: '需在模拟器中登录百度网盘账号后使用百度线路' },
  ali: { name: '阿里云盘', desc: '模拟器中登录阿里云盘' },
  tianyi: { name: '天翼云盘', desc: '模拟器中登录天翼云盘' },
  '115': { name: '115 网盘', desc: '模拟器中登录 115 网盘' },
  yidong: { name: '移动云盘', desc: '模拟器中登录移动云盘' },
};

export default function CloudAccountSettings({
  showAlert,
}: {
  showAlert: (
    type: 'success' | 'error' | 'warning',
    title: string,
    message?: string,
  ) => void;
}) {
  const [launchingDrive, setLaunchingDrive] = useState<string | null>(null);

  const drives = Object.keys(DRIVE_META);

  const launchLogin = async (drive: string) => {
    setLaunchingDrive(drive);
    try {
      await invoke('netdisk_launch_login', { drive });
      showAlert(
        'success',
        '已在模拟器中打开登录页',
        '请在模拟器窗口完成登录，登录成功后 cookie 自动生效',
      );
    } catch (e) {
      showAlert(
        'error',
        '打开登录页失败',
        e instanceof Error ? e.message : String(e),
      );
    } finally {
      setLaunchingDrive(null);
    }
  };

  return (
    <div className='space-y-4'>
      <p className='text-xs text-gray-500 dark:text-gray-400'>
        网盘资源播放需要对应网盘账号登录。点「在模拟器中登录」，在模拟器窗口内完成登录
        (扫码或账号密码均可)；登录态由模拟器内的桥接保存，播放网盘源时自动使用。
      </p>

      {drives.map((drive) => {
        const meta = DRIVE_META[drive];
        return (
          <div
            key={drive}
            className='flex flex-wrap items-center gap-3 rounded-lg border border-gray-200 p-3 dark:border-gray-700'
          >
            <div className='min-w-0 flex-1'>
              <div className='text-sm font-medium text-gray-900 dark:text-gray-100'>
                {meta.name}
              </div>
              <div className='mt-0.5 text-xs text-gray-500 dark:text-gray-400'>
                {meta.desc}
              </div>
            </div>
            <button
              onClick={() => launchLogin(drive)}
              disabled={launchingDrive === drive}
              className='rounded-lg bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-700 disabled:opacity-60'
            >
              {launchingDrive === drive
                ? '正在打开...'
                : '在模拟器中登录'}
            </button>
          </div>
        );
      })}
    </div>
  );
}
