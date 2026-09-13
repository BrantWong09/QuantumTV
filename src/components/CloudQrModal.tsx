'use client';

import { invoke } from '@tauri-apps/api/core';
import QRCode from 'qrcode';
import { useEffect, useState } from 'react';

type QrKind = { kind: 'Text'; data: string } | { kind: 'PngBase64'; data: string };
type SessionDto = { drive: string; qr: QrKind; token: string; cas_cookies: string[] };
type VerifyDto = {
  provider: string;
  ok: boolean;
  account: string | null;
  message: string;
  bridge_pushed: boolean;
};
type PollDto =
  | { status: 'waiting' }
  | { status: 'scanned' }
  | { status: 'confirmed'; data: { verify: VerifyDto } }
  | { status: 'expired' };

const SCAN_HINT: Record<string, string> = {
  quark: '请用手机「夸克App」内扫码并在App中确认',
  uc: '请用手机「UC浏览器/UC网盘App」内扫码并确认',
  baidu: '请用手机「百度App」扫码并确认',
};

const STATUS_TEXT: Record<string, string> = {
  verifying: '登录确认成功, 正在验证账号…',
  confirmed: '登录成功 (已验证)',
  unverified: '已保存但验证未通过',
  expired: '二维码已过期',
};

// 二维码生命周期 (§8/§9): 后端 60s 未确认视为过期, 前端同步兜底
const QR_LIFETIME_MS = 60_000;

function pollDelay(elapsedMs: number): number {
  // §9: 0~10s → 1s, 10~60s → 2s
  return elapsedMs < 10_000 ? 1000 : 2000;
}

export default function CloudQrModal({
  drive,
  driveName,
  onClose,
  showAlert,
  onLoggedIn,
}: {
  drive: string;
  driveName: string;
  onClose: () => void;
  showAlert: (
    type: 'success' | 'error' | 'warning',
    title: string,
    message?: string,
  ) => void;
  onLoggedIn?: () => void;
}) {
  const [qrSrc, setQrSrc] = useState<string | null>(null);
  const [status, setStatus] = useState<
    | 'loading'
    | 'waiting'
    | 'scanned'
    | 'verifying'
    | 'confirmed'
    | 'unverified'
    | 'expired'
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
        const startedAt = Date.now();

        const loop = async () => {
          if (!alive) return;
          const elapsed = Date.now() - startedAt;
          if (elapsed > QR_LIFETIME_MS) {
            setStatus('expired');
            return;
          }
          try {
            const r: PollDto = await invoke('cloud_login_poll', { session });
            if (!alive) return;
            failCount = 0;
            if (r.status === 'confirmed') {
              const v = r.data.verify;
              if (v.ok) {
                setStatus('confirmed');
                onLoggedIn?.();
                showAlert(
                  'success',
                  `${driveName} 登录成功`,
                  v.account ? `已验证账号: ${v.account}` : v.message,
                );
                timer = setTimeout(onClose, 1500);
              } else {
                // §6: 扫码确认 ≠ 登录成功 — 验证未通过时明确告知, 不显示成功
                setStatus('unverified');
                onLoggedIn?.();
                showAlert('warning', `${driveName} 验证未通过`, v.message);
              }
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
            failCount += 1;
            if (failCount >= 5) {
              showAlert('error', '轮询失败', msg);
              return;
            }
            setStatus('waiting');
          }
          timer = setTimeout(loop, pollDelay(Date.now() - startedAt));
        };
        timer = setTimeout(loop, pollDelay(0));
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
  const statusLine =
    status === 'loading'
      ? '正在获取二维码…'
      : status === 'waiting'
        ? SCAN_HINT[drive] ?? ''
        : status === 'scanned'
          ? '已扫码, 请在手机上确认'
          : STATUS_TEXT[status] ?? '';

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
          <span
            className={`text-xs ${
              status === 'unverified'
                ? 'text-amber-600 dark:text-amber-400'
                : 'text-gray-500 dark:text-gray-400'
            }`}
          >
            {statusLine}
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
