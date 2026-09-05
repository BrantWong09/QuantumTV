'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import QRCode from 'qrcode';
import { invoke } from '@tauri-apps/api/core';

interface CloudAccount {
  drive: string;
  cookie: string;
  updated_at: number;
  nickname: string;
}

interface ScanSession {
  drive: string;
  token: string;
  qr_content: string;
}

interface ScanPollResponse {
  state: string;
  account: CloudAccount | null;
}

const DRIVE_META: Record<
  string,
  { name: string; scan: boolean; hint: string }
> = {
  quark: {
    name: '夸克网盘',
    scan: true,
    hint: '推荐扫码: 手机夸克 App 扫二维码确认登录',
  },
  uc: {
    name: 'UC 网盘',
    scan: false,
    hint: '浏览器登录 drive.uc.cn 后从 F12 Network 复制 Cookie 粘贴',
  },
  baidu: {
    name: '百度网盘',
    scan: false,
    hint: '浏览器登录 pan.baidu.com 后复制 Cookie(需含 BDUSS)',
  },
  ali: {
    name: '阿里云盘',
    scan: false,
    hint: '粘贴网页版 Cookie 或 refresh_token',
  },
  tianyi: { name: '天翼云盘', scan: false, hint: '粘贴 Cookie' },
  '115': { name: '115 网盘', scan: false, hint: '粘贴 Cookie' },
  yidong: { name: '移动云盘', scan: false, hint: '粘贴 Cookie' },
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
  const [accounts, setAccounts] = useState<CloudAccount[]>([]);
  const [pasteOpen, setPasteOpen] = useState<string | null>(null);
  const [pasteText, setPasteText] = useState('');
  const [scan, setScan] = useState<{
    qrDataUrl: string;
    session: ScanSession;
  } | null>(null);
  const [scanState, setScanState] = useState('');
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const load = useCallback(async () => {
    try {
      setAccounts(await invoke<CloudAccount[]>('netdisk_get_accounts'));
    } catch {
      /* 首次为空 */
    }
  }, []);

  useEffect(() => {
    load();
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, [load]);

  const stopPolling = () => {
    if (pollRef.current) clearInterval(pollRef.current);
    pollRef.current = null;
  };

  const startScan = async (drive: string) => {
    try {
      const session = await invoke<ScanSession>('netdisk_scan_start', { drive });
      const qrDataUrl = await QRCode.toDataURL(session.qr_content, {
        width: 220,
        margin: 1,
      });
      setScan({ qrDataUrl, session });
      setScanState('请用手机夸克 App 扫码');
      stopPolling();
      pollRef.current = setInterval(async () => {
        try {
          const resp = await invoke<ScanPollResponse>('netdisk_scan_poll', {
            drive,
            token: session.token,
          });
          if (resp.state === 'confirmed') {
            stopPolling();
            setScan(null);
            setScanState('');
            showAlert('success', '登录成功', '网盘账号已保存, cookie 将自动续期');
            load();
          }
        } catch (e) {
          stopPolling();
          setScan(null);
          showAlert(
            'error',
            '扫码失败',
            e instanceof Error ? e.message : String(e),
          );
        }
      }, 2000);
    } catch (e) {
      showAlert(
        'error',
        '扫码发起失败',
        e instanceof Error ? e.message : String(e),
      );
    }
  };

  const savePaste = async (drive: string) => {
    try {
      await invoke('netdisk_save_cookie', { drive, cookie: pasteText });
      setPasteOpen(null);
      setPasteText('');
      showAlert('success', '保存成功', 'Cookie 已保存并注入播放链路');
      load();
    } catch (e) {
      showAlert(
        'error',
        '保存失败',
        e instanceof Error ? e.message : String(e),
      );
    }
  };

  const removeAccount = async (drive: string) => {
    try {
      await invoke('netdisk_delete_account', { drive });
      load();
    } catch (e) {
      showAlert('error', '删除失败', String(e));
    }
  };

  const refreshNow = async (drive: string) => {
    try {
      await invoke('netdisk_refresh_now', { drive });
      showAlert('success', '续期成功', 'Cookie 已刷新');
      load();
    } catch (e) {
      showAlert(
        'warning',
        '续期失败',
        e instanceof Error ? e.message : String(e),
      );
      load();
    }
  };

  const drives = Object.keys(DRIVE_META);

  return (
    <div className='space-y-4'>
      <p className='text-xs text-gray-500 dark:text-gray-400'>
        网盘资源播放需要登录网盘账号。夸克支持 App 扫码登录；其他网盘粘贴
        Cookie。登录后 Cookie 自动续期并注入播放链路。
      </p>

      {drives.map((drive) => {
        const meta = DRIVE_META[drive];
        const acc = accounts.find((a) => a.drive === drive);
        const loggedAt = acc
          ? new Date(acc.updated_at * 1000).toLocaleString()
          : null;
        return (
          <div
            key={drive}
            className='rounded-lg border border-gray-200 p-3 dark:border-gray-700'
          >
            <div className='flex flex-wrap items-center gap-2'>
              <span
                className={`inline-block h-2.5 w-2.5 rounded-full ${
                  acc ? 'bg-green-500' : 'bg-gray-300 dark:bg-gray-600'
                }`}
              />
              <span className='text-sm font-medium text-gray-900 dark:text-gray-100'>
                {meta.name}
              </span>
              {acc && (
                <span className='text-xs text-gray-500 dark:text-gray-400'>
                  已登录 · {loggedAt}
                </span>
              )}
              <div className='ml-auto flex gap-2'>
                {meta.scan && (
                  <button
                    onClick={() => startScan(drive)}
                    className='rounded-lg bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-700'
                  >
                    {acc ? '重新扫码' : '扫码登录'}
                  </button>
                )}
                {!meta.scan && (
                  <button
                    onClick={() => {
                      setPasteOpen(pasteOpen === drive ? null : drive);
                      setPasteText('');
                    }}
                    className='rounded-lg bg-blue-600 px-3 py-1.5 text-xs text-white hover:bg-blue-700'
                  >
                    {acc ? '更新 Cookie' : '粘贴 Cookie'}
                  </button>
                )}
                {acc && drive === 'quark' && (
                  <button
                    onClick={() => refreshNow(drive)}
                    className='rounded-lg border border-gray-300 px-3 py-1.5 text-xs text-gray-700 hover:bg-gray-50 dark:border-gray-600 dark:text-gray-300 dark:hover:bg-gray-700'
                  >
                    立即续期
                  </button>
                )}
                {acc && (
                  <button
                    onClick={() => removeAccount(drive)}
                    className='rounded-lg border border-red-300 px-3 py-1.5 text-xs text-red-600 hover:bg-red-50 dark:border-red-800 dark:hover:bg-red-900/30'
                  >
                    退出
                  </button>
                )}
              </div>
            </div>
            <p className='mt-1 text-xs text-gray-500 dark:text-gray-400'>
              {meta.hint}
            </p>

            {pasteOpen === drive && (
              <div className='mt-2'>
                <textarea
                  value={pasteText}
                  onChange={(e) => setPasteText(e.target.value)}
                  rows={3}
                  placeholder='粘贴完整 Cookie 串'
                  className='w-full rounded-lg border border-gray-300 px-3 py-2 text-xs dark:border-gray-600 dark:bg-gray-700 dark:text-gray-100'
                />
                <button
                  onClick={() => savePaste(drive)}
                  className='mt-1 rounded-lg bg-green-600 px-3 py-1.5 text-xs text-white hover:bg-green-700'
                >
                  保存
                </button>
              </div>
            )}

            {scan && scan.session.drive === drive && (
              <div className='mt-3 flex flex-col items-center rounded-lg bg-gray-50 p-3 dark:bg-gray-900/50'>
                <img src={scan.qrDataUrl} alt='扫码二维码' className='h-[220px] w-[220px]' />
                <p className='mt-2 text-sm text-gray-700 dark:text-gray-300'>
                  {scanState}
                </p>
                <button
                  onClick={() => {
                    stopPolling();
                    setScan(null);
                  }}
                  className='mt-2 text-xs text-gray-500 underline'
                >
                  取消
                </button>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
