'use client';

import { invoke } from '@tauri-apps/api/core';
import { useCallback, useEffect, useRef, useState } from 'react';

interface BridgeSettingsDto {
  remote_url: string;
}

interface BridgeStatusDto {
  status: 'idle' | 'starting' | 'ready' | 'failed';
  effective_url: string | null;
  mode: 'none' | 'remote' | 'tunnel';
}

const STATUS_LABEL: Record<BridgeStatusDto['status'], string> = {
  idle: '未启动',
  starting: '启动中',
  ready: '就绪',
  failed: '失败',
};

const STATUS_COLOR: Record<BridgeStatusDto['status'], string> = {
  idle: 'bg-gray-400',
  starting: 'bg-yellow-500',
  ready: 'bg-green-500',
  failed: 'bg-red-500',
};

const MODE_LABEL: Record<BridgeStatusDto['mode'], string> = {
  none: '无',
  remote: '远程直连',
  tunnel: '模拟器隧道',
};

export default function BridgeSettings({
  showAlert,
}: {
  showAlert: (
    type: 'success' | 'error' | 'warning',
    title: string,
    message?: string,
  ) => void;
}) {
  const [settings, setSettings] = useState<BridgeSettingsDto>({
    remote_url: '',
  });
  const [status, setStatus] = useState<BridgeStatusDto | null>(null);
  const [saving, setSaving] = useState(false);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const refreshStatus = useCallback(async () => {
    try {
      setStatus(await invoke<BridgeStatusDto>('get_bridge_status'));
    } catch {
      /* 状态查询失败不打断 */
    }
  }, []);

  const startPolling = useCallback(() => {
    if (pollRef.current) clearInterval(pollRef.current);
    const startedAt = Date.now();
    pollRef.current = setInterval(async () => {
      try {
        const s = await invoke<BridgeStatusDto>('get_bridge_status');
        setStatus(s);
        if (
          s.status === 'ready' ||
          s.status === 'failed' ||
          Date.now() - startedAt > 120000
        ) {
          if (pollRef.current) clearInterval(pollRef.current);
          pollRef.current = null;
        }
      } catch {
        /* 忽略单次轮询失败 */
      }
    }, 1000);
  }, []);

  useEffect(() => {
    (async () => {
      try {
        setSettings(await invoke<BridgeSettingsDto>('get_bridge_config'));
      } catch {
        /* 读取失败用默认值 */
      }
      await refreshStatus();
    })();
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [refreshStatus]);

  const handleSave = async () => {
    setSaving(true);
    try {
      await invoke('save_bridge_config', { settings });
      showAlert('success', '保存成功', '桥接正在重新连接');
      startPolling();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (message.includes('桥接正在启动中')) {
        // 落盘已成功，仅重试被 Starting 防重入拒绝，不算保存失败
        showAlert('warning', '已保存', '桥接正在启动中，稍后可手动重试');
      } else {
        showAlert('error', '保存失败', message);
      }
    } finally {
      setSaving(false);
    }
  };

  const handleRetry = async () => {
    try {
      await invoke('retry_bridge');
      startPolling();
    } catch (error) {
      showAlert(
        'error',
        '重试失败',
        error instanceof Error ? error.message : String(error),
      );
    }
  };

  return (
    <div className='space-y-4'>
      {/* 模拟器隧道 */}
      <div className='rounded-lg border border-gray-200 p-3 text-sm dark:border-gray-700'>
        <div className='font-medium text-gray-900 dark:text-gray-100'>模拟器隧道（推荐）</div>
        <p className='mt-1 text-xs text-gray-500 dark:text-gray-400'>
          将项目内{' '}
          <code className='rounded bg-gray-100 px-1 dark:bg-gray-800'>
            android/spider-bridge/out/bridge.apk
          </code>{' '}
          拖入模拟器窗口安装并保持运行，桥接自动建立（无需 adb）。
          模拟器重启后服务自启，隧道自动重连。
        </p>
      </div>

      {/* 远程桥接地址 */}
      <div>
        <label className='mb-1 block text-sm font-medium text-gray-700 dark:text-gray-300'>
          远程桥接地址
        </label>
        <input
          type='text'
          value={settings.remote_url}
          onChange={(e) =>
            setSettings({ ...settings, remote_url: e.target.value })
          }
          placeholder='http://192.168.1.20:8080'
          className='w-full rounded-lg border border-gray-300 px-3 py-2 text-sm dark:border-gray-600 dark:bg-gray-700 dark:text-gray-100'
        />
        <p className='mt-1 text-xs text-gray-500 dark:text-gray-400'>
          在同一局域网设备（电视盒子/旧手机）安装 QuantumTV Bridge APK
          后填入其地址；未鉴权，仅限可信局域网使用。
        </p>
      </div>

      {/* 状态卡片 */}
      <div className='rounded-lg border border-gray-200 p-3 text-sm dark:border-gray-700'>
        {status ? (
          <div className='flex items-center gap-2'>
            <span
              className={`inline-block h-2.5 w-2.5 rounded-full ${STATUS_COLOR[status.status]}`}
            />
            <span className='font-medium'>{STATUS_LABEL[status.status]}</span>
            <span className='text-gray-500 dark:text-gray-400'>
              模式: {MODE_LABEL[status.mode]}
            </span>
            {status.effective_url && (
              <span className='text-gray-500 dark:text-gray-400'>
                {status.effective_url}
              </span>
            )}
          </div>
        ) : (
          <span className='text-gray-500 dark:text-gray-400'>加载中…</span>
        )}
      </div>

      {/* 操作 */}
      <div className='flex gap-2'>
        <button
          onClick={handleSave}
          disabled={saving}
          className='rounded-lg bg-blue-600 px-4 py-2 text-sm text-white transition-colors hover:bg-blue-700 disabled:opacity-50'
        >
          {saving ? '保存中…' : '保存并重连'}
        </button>
        <button
          onClick={handleRetry}
          className='rounded-lg border border-gray-300 px-4 py-2 text-sm text-gray-700 transition-colors hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700'
        >
          重试
        </button>
      </div>
    </div>
  );
}
