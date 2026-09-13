'use client';

import { invoke } from '@tauri-apps/api/core';
import { useCallback, useEffect, useState } from 'react';

import CloudQrModal from './CloudQrModal';

type AuthStateDto = {
  provider: string;
  status:
    | 'unknown'
    | 'unauthenticated'
    | 'qr_pending'
    | 'authenticated'
    | 'expired'
    | 'invalid'
    | 'refreshing';
  account: string | null;
  created_at_ms: number | null;
  expires_at_ms: number | null;
  last_verified_at_ms: number | null;
  last_error: string | null;
};

type CheckItemDto = { name: string; ok: boolean; detail: string };
type ConnectionTestDto = {
  provider: string;
  ok: boolean;
  items: CheckItemDto[];
};

const DRIVE_META: Record<
  string,
  { name: string; desc: string }
> = {
  quark: { name: '夸克网盘', desc: '桌面端扫码登录（推荐），或备用模拟器 WebView 登录' },
  uc: { name: 'UC 网盘', desc: '桌面端扫码登录（推荐），或备用模拟器 WebView 登录' },
  baidu: { name: '百度网盘', desc: '桌面端扫码登录（推荐），或备用模拟器 WebView 登录' },
  ali: { name: '阿里云盘', desc: '模拟器中登录阿里云盘' },
  tianyi: { name: '天翼云盘', desc: '模拟器中登录天翼云盘' },
  '115': { name: '115 网盘', desc: '模拟器中登录 115 网盘' },
  yidong: { name: '移动云盘', desc: '模拟器中登录移动云盘' },
};

const QR_DRIVES: Record<string, string> = {
  quark: '夸克网盘',
  uc: 'UC 网盘',
  baidu: '百度网盘',
};

// §30: 状态点与文案 — "已登录" 只在 Authenticated 出现, 验证失败/过期单独标出
const STATE_META: Record<
  string,
  { dot: string; label: string; playback: string }
> = {
  authenticated: {
    dot: 'bg-green-500',
    label: '已登录',
    playback: '播放能力: 正常',
  },
  expired: {
    dot: 'bg-amber-500',
    label: '登录已过期',
    playback: '播放能力: 需要重新扫码',
  },
  invalid: {
    dot: 'bg-red-500',
    label: '登录状态无效',
    playback: '播放能力: 需要重新验证',
  },
  qr_pending: {
    dot: 'bg-blue-400',
    label: '等待扫码确认',
    playback: '',
  },
  refreshing: {
    dot: 'bg-blue-400',
    label: '正在刷新登录态',
    playback: '',
  },
  unauthenticated: {
    dot: 'bg-gray-400',
    label: '未登录',
    playback: '',
  },
  unknown: {
    dot: 'bg-gray-400',
    label: '未验证',
    playback: '播放能力: 需要验证',
  },
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
  const [qrDrive, setQrDrive] = useState<string | null>(null);
  const [states, setStates] = useState<Record<string, AuthStateDto>>({});
  const [testingDrive, setTestingDrive] = useState<string | null>(null);

  const refreshStates = useCallback(async () => {
    try {
      const list = await invoke<AuthStateDto[]>('cloud_login_states');
      const map: Record<string, AuthStateDto> = {};
      for (const s of list) map[s.provider] = s;
      setStates(map);
    } catch {
      // 后端未初始化时静默 (显示为未登录)
    }
  }, []);

  useEffect(() => {
    refreshStates();
  }, [refreshStates]);

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

  const testConnection = async (drive: string) => {
    setTestingDrive(drive);
    try {
      const r = await invoke<ConnectionTestDto>('cloud_login_test', {
        drive,
        probe: null,
      });
      const lines = r.items
        .map((i) => `${i.ok ? '✓' : '✗'} ${i.name}: ${i.detail}`)
        .join('\n');
      if (r.ok) {
        showAlert('success', `${DRIVE_META[drive].name} 连接正常`, lines);
      } else {
        showAlert('warning', `${DRIVE_META[drive].name} 检查未全部通过`, lines);
      }
      refreshStates();
    } catch (e) {
      showAlert('error', '测试连接失败', e instanceof Error ? e.message : String(e));
    } finally {
      setTestingDrive(null);
    }
  };

  const logout = async (drive: string) => {
    try {
      await invoke('cloud_login_logout', { drive });
      showAlert('success', '已退出登录', '本机凭证已删除 (模拟器内 cookie 不受影响)');
      refreshStates();
    } catch (e) {
      showAlert('error', '退出失败', e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className='space-y-4'>
      <p className='text-xs text-gray-500 dark:text-gray-400'>
        网盘资源播放需要对应网盘账号登录。夸克/UC/百度推荐直接在桌面端扫码
        （手机网盘 App 扫码确认即可，无需操作模拟器）；其余网盘在模拟器窗口内完成登录。
        凭证在本机加密保存，重启后自动恢复；播放异常会自动重新解析地址。
      </p>

      {drives.map((drive) => {
        const meta = DRIVE_META[drive];
        const qrName = QR_DRIVES[drive];
        const state = states[drive];
        const sm = STATE_META[state?.status ?? 'unauthenticated'];
        return (
          <div
            key={drive}
            className='rounded-lg border border-gray-200 p-3 dark:border-gray-700'
          >
            <div className='flex flex-wrap items-center gap-3'>
              <div className='min-w-0 flex-1'>
                <div className='flex items-center gap-2'>
                  <span
                    className={`inline-block h-2 w-2 shrink-0 rounded-full ${sm.dot}`}
                  />
                  <span className='text-sm font-medium text-gray-900 dark:text-gray-100'>
                    {meta.name}
                  </span>
                  {qrName && (
                    <span className='text-xs text-gray-500 dark:text-gray-400'>
                      {sm.label}
                      {state?.account ? ` (${state.account})` : ''}
                    </span>
                  )}
                </div>
                <div className='mt-0.5 text-xs text-gray-500 dark:text-gray-400'>
                  {qrName ? sm.playback || meta.desc : meta.desc}
                  {state?.last_error && state.status !== 'authenticated' && (
                    <span className='ml-2 text-red-500 dark:text-red-400'>
                      {state.last_error}
                    </span>
                  )}
                </div>
              </div>
              {qrName ? (
                <div className='flex gap-2'>
                  <button
                    onClick={() => setQrDrive(drive)}
                    className='rounded-lg bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-700'
                  >
                    {state && state.status !== 'unauthenticated' ? '重新扫码' : '扫码登录'}
                  </button>
                  {state && state.status !== 'unauthenticated' && (
                    <>
                      <button
                        onClick={() => testConnection(drive)}
                        disabled={testingDrive === drive}
                        className='rounded-lg border border-gray-300 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50 disabled:opacity-60 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800'
                      >
                        {testingDrive === drive ? '测试中...' : '测试连接'}
                      </button>
                      <button
                        onClick={() => logout(drive)}
                        className='rounded-lg border border-gray-300 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800'
                      >
                        退出登录
                      </button>
                    </>
                  )}
                  <button
                    onClick={() => launchLogin(drive)}
                    disabled={launchingDrive === drive}
                    className='rounded-lg border border-gray-300 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-50 disabled:opacity-60 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-800'
                  >
                    {launchingDrive === drive ? '正在打开...' : '模拟器登录'}
                  </button>
                </div>
              ) : (
                <button
                  onClick={() => launchLogin(drive)}
                  disabled={launchingDrive === drive}
                  className='rounded-lg bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-700 disabled:opacity-60'
                >
                  {launchingDrive === drive ? '正在打开...' : '在模拟器中登录'}
                </button>
              )}
            </div>
          </div>
        );
      })}

      {qrDrive && (
        <CloudQrModal
          drive={qrDrive}
          driveName={QR_DRIVES[qrDrive]}
          onClose={() => setQrDrive(null)}
          showAlert={showAlert}
          onLoggedIn={refreshStates}
        />
      )}
    </div>
  );
}
