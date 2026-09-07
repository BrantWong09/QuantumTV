'use client';

import { invoke } from '@tauri-apps/api/core';
import QRCode from 'qrcode';
import { useEffect, useState } from 'react';

type QrKind = { kind: 'Text'; data: string } | { kind: 'PngBase64'; data: string };
type SessionDto = { drive: string; qr: QrKind; token: string; cas_cookies: string[] };
type PollDto =
  | { status: 'waiting' }
  | { status: 'scanned' }
  | { status: 'confirmed'; data: { cookie: string } }
  | { status: 'expired' };

const STATUS_TEXT: Record<string, string> = {
  waiting: '等待扫码…（请用对应网盘 App 扫码）',
  scanned: '已扫码, 请在手机上确认',
  confirmed: '登录成功',
  expired: '二维码已过期',
};

export default function CloudQrModal({
  drive,
  driveName,
  onClose,
  showAlert,
}: {
  drive: string;
  driveName: string;
  onClose: () => void;
  showAlert: (
    type: 'success' | 'error' | 'warning',
    title: string,
    message?: string,
  ) => void;
}) {
  const [qrSrc, setQrSrc] = useState<string | null>(null);
  const [status, setStatus] = useState<
    'loading' | 'waiting' | 'scanned' | 'confirmed' | 'expired'
  >('loading');
  const [runId, setRunId] = useState(0);

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let failCount = 0;

    (async () => {
      try {
        const session: SessionDto = await invoke('cloud_login_start', { drive });
        if (!alive) return;
        if (session.qr.kind === 'Text') {
          const url = await QRCode.toDataURL(session.qr.data, {
            width: 320,
            margin: 1,
          });
          if (alive) setQrSrc(url);
        } else {
          if (alive) setQrSrc(`data:image/png;base64,${session.qr.data}`);
        }
        setStatus('waiting');

        const loop = async () => {
          if (!alive) return;
          try {
            const r: PollDto = await invoke('cloud_login_poll', { session });
            if (!alive) return;
            failCount = 0;
            if (r.status === 'confirmed') {
              setStatus('confirmed');
              showAlert('success', `${driveName} 登录成功`, '账号 cookie 已写入模拟器桥接');
              timer = setTimeout(onClose, 1500);
              return;
            }
            if (r.status === 'expired') {
              setStatus('expired');
              return;
            }
            setStatus(r.status);
          } catch (e) {
            if (!alive) return;
            const msg = e instanceof Error ? e.message : String(e);
            if (msg.includes('桥接未就绪') || msg.includes('推送桥接失败')) {
              showAlert('error', '写入模拟器失败', msg);
              return;
            }
            failCount += 1;
            if (failCount >= 5) {
              showAlert('error', '轮询失败', msg);
              return;
            }
            setStatus('waiting');
          }
          timer = setTimeout(loop, 2000);
        };
        timer = setTimeout(loop, 2000);
      } catch (e) {
        if (!alive) return;
        showAlert('error', '获取二维码失败', e instanceof Error ? e.message : String(e));
      }
    })();

    return () => {
      alive = false;
      if (timer) clearTimeout(timer);
    };
    // showAlert/onClose 由父组件保证稳定; drive/runId 变化即重启流程
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [drive, runId]);

  const expired = status === 'expired';

  return (
    <div
      className='fixed inset-0 z-50 flex items-center justify-center bg-black/60'
      onClick={onClose}
    >
      <div
        className='w-96 rounded-xl bg-white p-5 shadow-2xl dark:bg-gray-900'
        onClick={(e) => e.stopPropagation()}
      >
        <div className='mb-3 flex items-center justify-between'>
          <h3 className='text-base font-medium text-gray-900 dark:text-gray-100'>
            {driveName} 扫码登录
          </h3>
          <button
            onClick={onClose}
            className='text-gray-400 hover:text-gray-600 dark:hover:text-gray-200'
          >
            ✕
          </button>
        </div>
        <div className='flex h-80 items-center justify-center rounded-lg bg-white'>
          {qrSrc ? (
            // eslint-disable-next-line @next/next/no-img-element
            <img src={qrSrc} alt='登录二维码' className='h-72 w-72 object-contain' />
          ) : (
            <span className='text-sm text-gray-400'>正在获取二维码…</span>
          )}
        </div>
        <div className='mt-3 flex items-center justify-between'>
          <span className='text-xs text-gray-500 dark:text-gray-400'>
            {STATUS_TEXT[status]}
          </span>
          {expired && (
            <button
              onClick={() => {
                setQrSrc(null);
                setStatus('loading');
                setRunId((n) => n + 1);
              }}
              className='rounded-lg bg-blue-600 px-3 py-1 text-xs text-white hover:bg-blue-700'
            >
              刷新二维码
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
