'use client';

import { useEffect, useRef, useState } from 'react';
import { Download, RefreshCw } from 'lucide-react';
import { Button } from './button';
import type { LauncherBridge, UpdateComponent, UpdateStatus } from '@/lib/runtime';

function failureText(failure: unknown, fallback: string): string {
  const message = failure instanceof Error ? failure.message : typeof failure === 'string' ? failure : null;
  return message && message.length <= 1000 ? message : fallback;
}

function downloadSize(bytes: number | null): string {
  if (bytes === null) return '';
  return `${(bytes / (1024 * 1024)).toLocaleString('zh-TW', { maximumFractionDigits: 1 })} MB`;
}

function UpdateRow({ component, configured, repository, current, available, bytes, detail, disabled, installing, install }: {
  component: UpdateComponent;
  configured: boolean;
  repository: string | null;
  current: string | null;
  available: string | null;
  bytes: number | null;
  detail: string;
  disabled: boolean;
  installing: boolean;
  install: (component: UpdateComponent) => void;
}) {
  const name = component === 'launcher' ? 'Kaleidoscope' : 'Transfiguration';
  return (
    <div className="update-row" data-update-component={component}>
      <div className="update-copy">
        <div className="update-identity"><strong>{name}</strong><span>{current ? current === '介面預覽' ? current : `v${current}` : '未讀取版本'}</span></div>
        <p>{available ? `可更新至 v${available}${bytes === null ? '' : ` · ${downloadSize(bytes)}`}` : detail}</p>
        {configured && repository && <span className="update-source" title={repository}>GitHub · {repository}</span>}
      </div>
      {available && <Button variant="outline" className="update-button" disabled={disabled || !configured} onClick={() => install(component)} aria-label={`更新 ${name}`}>
        <Download size={13} aria-hidden="true" />{installing ? '正在安裝' : '更新'}
      </Button>}
    </div>
  );
}

export function Updates({ bridge, installBlocked, beginInstall, endInstall }: {
  bridge: LauncherBridge | null;
  installBlocked: boolean;
  beginInstall: () => boolean;
  endInstall: () => void;
}) {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState<UpdateComponent | null>(null);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(false);
  const requested = useRef(false);
  const applying = useRef(false);
  const checkedRuntime = useRef<LauncherBridge | null>(null);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    if (!bridge) return;
    let active = true;
    void (async () => {
      try {
        const local = await bridge.updateStatus();
        if (!active) return;
        setStatus(local);
        setError(null);
        if (bridge.mode === 'native' && (local.configured || local.bridgeConfigured) && checkedRuntime.current !== bridge) {
          checkedRuntime.current = bridge;
          requested.current = true;
          setChecking(true);
          try {
            const checked = await bridge.checkUpdates();
            if (active) setStatus(checked);
          } catch (failure) {
            if (active) setError(failureText(failure, '無法檢查 GitHub 更新。請稍後重試。'));
          } finally {
            requested.current = false;
            if (active) setChecking(false);
          }
        }
      } catch (failure) {
        if (active) setError(failureText(failure, '無法讀取更新資訊。請重試讀取。'));
      }
    })();
    return () => { active = false; };
  }, [bridge]);

  async function check() {
    if (!bridge || requested.current || applying.current) return;
    requested.current = true;
    setChecking(true);
    setError(null);
    try {
      const next = status ? await bridge.checkUpdates() : await bridge.updateStatus();
      if (mounted.current) setStatus(next);
    } catch (failure) {
      if (mounted.current) setError(failureText(failure, '無法檢查 GitHub 更新。請稍後重試。'));
    } finally {
      requested.current = false;
      if (mounted.current) setChecking(false);
    }
  }

  async function install(component: UpdateComponent) {
    if (!bridge || bridge.mode !== 'native' || !bridge.interactive || installBlocked || requested.current || applying.current
      || !(component === 'launcher' ? status?.availableVersion : status?.bridgeAvailableVersion) || !beginInstall()) return;
    applying.current = true;
    setInstalling(component);
    setError(null);
    try {
      await bridge.installUpdate(component);
      // Native installation exits and restarts the launcher; keep actions locked until then.
    } catch (failure) {
      applying.current = false;
      endInstall();
      if (mounted.current) {
        setInstalling(null);
        setError(failureText(failure, '更新未安裝。請稍後重試。'));
      }
    }
  }

  const configured = Boolean(status?.configured || status?.bridgeConfigured);
  const disabled = installBlocked || checking || installing !== null || bridge?.mode !== 'native';
  const blockingHint = installBlocked && (status?.availableVersion || status?.bridgeAvailableVersion)
    ? '請先完成目前操作、關閉遊戲並清理連線模組，再安裝更新。' : null;

  return (
    <section className="updates" aria-labelledby="updates-title" aria-busy={checking || installing !== null}>
      <div className="updates-heading">
        <h2 id="updates-title">程式更新</h2>
        {(configured || error) && <Button variant="quiet" className="update-button" disabled={!bridge || checking || installing !== null} onClick={() => void check()}>
          <RefreshCw size={13} aria-hidden="true" />{checking ? '正在檢查' : !status ? '重新讀取更新' : error ? '重試檢查' : '檢查更新'}
        </Button>}
      </div>
      {status ? <>
        <UpdateRow component="launcher" configured={status.configured} repository={status.repository} current={status.currentVersion}
          available={status.availableVersion} bytes={status.downloadBytes} detail={status.detail} disabled={disabled} installing={installing === 'launcher'} install={(component) => void install(component)} />
        <UpdateRow component="bridge" configured={status.bridgeConfigured} repository={status.bridgeRepository} current={status.bridgeVersion}
          available={status.bridgeAvailableVersion} bytes={status.bridgeDownloadBytes} detail={status.bridgeDetail} disabled={disabled} installing={installing === 'bridge'} install={(component) => void install(component)} />
      </> : !error && <p className="updates-loading">正在讀取更新資訊…</p>}
      <div className="updates-feedback" aria-live="polite" aria-atomic="true">
        {error && <p className="updates-error" role="alert">{error}</p>}
        {installing !== null ? <p>正在驗證並安裝更新，完成後會重新啟動啟動器。</p> : blockingHint && <p>{blockingHint}</p>}
      </div>
    </section>
  );
}
