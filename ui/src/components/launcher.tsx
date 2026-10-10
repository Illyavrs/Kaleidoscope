'use client';

import { useEffect, useRef, useState } from 'react';
import { Check, CircleHelp, FolderOpen, LoaderCircle, Play, RotateCcw, Square, TriangleAlert } from 'lucide-react';
import { Button } from './button';
import { ServerSettings } from './server-settings';
import { Updates } from './updates';
import { createLauncherBridge, emptySnapshot, type Client, type LauncherBridge, type LauncherCommand, type LauncherSettings, type LauncherSnapshot, type Phase } from '@/lib/runtime';

const labels: Record<Phase, string> = {
  idle: '準備就緒', preparing: '正在準備', running: '遊戲執行中', closing: '正在關閉遊戲',
  cleaning: '正在清理', cleanup_failed: '清理尚未完成', error: '需要處理',
};
const busyPhases: Phase[] = ['preparing', 'closing', 'cleaning'];

function ClientRow({ client, snapshot, endpoint, disabled, run, choose }: {
  client: Client; snapshot: LauncherSnapshot; disabled: boolean;
  endpoint: string | null;
  run: () => void; choose: () => void;
}) {
  const state = snapshot[client];
  const customStablePort = client === 'stable' && endpoint && Boolean(new URL(endpoint).port);
  const blocked = customStablePort || (client === 'lazer' && (snapshot.lazer.compatibility !== 'supported'
    || (snapshot.pluginPresent && !snapshot.lazer.running)));
  const linux = snapshot.platform === 'linux';
  const hint = customStablePort ? '目前網址使用自訂連接埠；stable 需要 HTTPS 443。'
    : client === 'stable' ? state.path ? '以 devserver 參數連到設定的伺服器。' : '未找到預設安裝位置，請選擇 osu!.exe。'
    : snapshot.lazer.compatibility === 'unsupported' ? '此版本暫不支援，請等待啟動器與連線 ruleset 配套更新。'
      : snapshot.lazer.compatibility === 'unknown' && state.path ? '正在確認此版本的相容性。'
        : linux && !state.path ? '選擇 osu.AppImage，或解壓縮後的 osu!。'
          : '啟動前放入連線 ruleset，關閉後自動清理。';

  return (
    <section className="client-row" aria-labelledby={`${client}-title`} data-client={client}>
      <div className="client-heading">
        <div><h2 id={`${client}-title`}>osu! <span>{client}</span></h2><p>{hint}</p></div>
        <span className={`client-state ${state.running ? 'is-running' : ''}`}>
          {state.running ? '執行中' : '未啟動'}
        </span>
      </div>
      <div className="path-row">
        <label className="sr-only" htmlFor={`${client}-path`}>{client} 安裝位置</label>
        <input id={`${client}-path`} readOnly value={state.path ?? ''} placeholder={client === 'stable' ? '未偵測到安裝位置' : '尚未選擇安裝位置'} title={state.path ?? ''} spellCheck={false} />
        <Button variant="outline" onClick={choose} disabled={disabled} aria-label={`選擇 ${client} 安裝位置`}>
          <FolderOpen size={15} aria-hidden="true" /><span>{state.path ? '變更位置' : '選擇位置'}</span>
        </Button>
      </div>
      <div className="client-action">
        <span>{client === 'lazer' ? '只會連線到設定的伺服器' : '既有 stable 客戶端'}</span>
        <Button onClick={run} disabled={disabled || !state.path || blocked} aria-label={`${state.running ? '關閉並重新啟動' : '啟動'} ${client}`}>
          {state.running ? <RotateCcw size={15} aria-hidden="true" /> : <Play size={15} aria-hidden="true" />}
          {state.running ? '關閉並重新啟動' : '啟動'}
        </Button>
      </div>
    </section>
  );
}

export function Launcher() {
  const [snapshot, setSnapshot] = useState<LauncherSnapshot>(emptySnapshot);
  const [bridge, setBridge] = useState<LauncherBridge | null>(null);
  const [pending, setPending] = useState(false);
  const [settings, setSettings] = useState<LauncherSettings | null>(null);
  const [settingsPending, setSettingsPending] = useState(false);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [updatePending, setUpdatePending] = useState(false);
  const updating = useRef(false);
  const readyRuntime = useRef<LauncherBridge | null>(null);
  const changingSettings = useRef(false);
  const mounted = useRef(false);
  const invoking = useRef(false);
  const sequence = useRef(0);

  useEffect(() => {
    mounted.current = true;
    let active = true;
    let timer: ReturnType<typeof setInterval> | undefined;
    let polling = false;
    createLauncherBridge().then(async (runtime) => {
      const state = await runtime.call('launcher_snapshot');
      if (!active) return;
      setBridge(runtime);
      setSnapshot(state);
      try {
        const preferences = await runtime.settings();
        if (active) { setSettings(preferences); setSettingsError(null); }
      } catch {
        if (active) setSettingsError('無法讀取伺服器設定。請重試讀取。');
      }
      if (!active) return;
      timer = setInterval(async () => {
        if (invoking.current || polling) return;
        polling = true;
        const revision = sequence.current;
        try {
          const next = await runtime.call('launcher_snapshot');
          if (active && revision === sequence.current && !invoking.current) setSnapshot(next);
        } catch {
          if (active && revision === sequence.current && !invoking.current) {
            setSnapshot((current) => ({ ...current, phase: 'error', detail: '無法取得遊戲狀態。請重新開啟啟動器。' }));
          }
        } finally {
          polling = false;
        }
      }, 2000);
    }).catch(() => {
      if (active) setSnapshot((current) => ({ ...current, phase: 'error', detail: '無法連接啟動器。請重新開啟桌面程式。' }));
    });
    return () => { active = false; mounted.current = false; if (timer) clearInterval(timer); };
  }, []);

  useEffect(() => {
    if (!bridge || bridge.mode !== 'native' || readyRuntime.current === bridge) return;
    let active = true;
    readyRuntime.current = bridge;
    // This effect runs after the validated snapshot and launcher content commit.
    void bridge.ready().catch(() => {
      if (active) setSnapshot((current) => ({ ...current, phase: 'error', detail: '新版介面無法完成啟動確認。請重新開啟啟動器。' }));
    });
    return () => { active = false; };
  }, [bridge]);

  async function action(command: LauncherCommand, client?: Client) {
    if (!bridge?.interactive || invoking.current || changingSettings.current || updating.current
      || (!settings && (command === 'launcher_start' || command === 'launcher_choose_path'))) return;
    invoking.current = true;
    sequence.current += 1;
    setPending(true);
    if (command === 'launcher_start') {
      setSnapshot((current) => ({
        ...current, phase: client && current[client].running ? 'closing' : 'preparing',
        detail: client && snapshot[client].running ? '先關閉遊戲，再重新準備連線。' : '正在檢查連線並準備遊戲。',
      }));
    } else if (command === 'launcher_stop' || command === 'launcher_recover') {
      setSnapshot((current) => ({ ...current, phase: command === 'launcher_stop' ? 'closing' : 'cleaning', detail: '請等待遊戲關閉與連線 ruleset 清理完成。' }));
    }
    try {
      const next = await bridge.call(command, client ? { client } : undefined);
      if (mounted.current) setSnapshot(next);
    } catch (error) {
      if (mounted.current) setSnapshot((current) => ({
        ...current, phase: command === 'launcher_recover' ? 'cleanup_failed' : 'error',
        detail: error instanceof Error && error.message.length <= 1000 ? error.message : '操作未完成。請確認遊戲位置後再試一次。',
      }));
    } finally {
      invoking.current = false;
      if (mounted.current) setPending(false);
    }
  }

  async function saveEndpoint(endpoint: string): Promise<LauncherSettings> {
    if (!bridge?.interactive || invoking.current || changingSettings.current || updating.current) throw new Error('請等待目前操作完成後再儲存。');
    changingSettings.current = true;
    setSettingsPending(true);
    try {
      const next = await bridge.saveEndpoint(endpoint);
      if (mounted.current) { setSettings(next); setSettingsError(null); }
      return next;
    } finally {
      changingSettings.current = false;
      if (mounted.current) setSettingsPending(false);
    }
  }

  async function retrySettings() {
    if (!bridge || changingSettings.current || updating.current) return;
    changingSettings.current = true;
    setSettingsPending(true);
    try {
      const next = await bridge.settings();
      if (mounted.current) { setSettings(next); setSettingsError(null); }
    } catch {
      if (mounted.current) setSettingsError('無法讀取伺服器設定。請重試讀取。');
    } finally {
      changingSettings.current = false;
      if (mounted.current) setSettingsPending(false);
    }
  }

  const busy = pending || updatePending || busyPhases.includes(snapshot.phase);
  const cleanupFailed = snapshot.phase === 'cleanup_failed';
  const blocked = !bridge?.interactive || busy || settingsPending || !settings || cleanupFailed;
  const running = snapshot.stable.running || snapshot.lazer.running;
  const error = cleanupFailed || snapshot.phase === 'error';
  const cleanupTitle = snapshot.phase === 'cleaning' ? '正在清理連線 ruleset'
    : cleanupFailed ? '連線 ruleset 尚未清理'
      : snapshot.pluginPresent ? '連線 ruleset 使用中' : '沒有暫存的連線 ruleset';

  function beginUpdate() {
    if (!bridge?.interactive || updating.current || invoking.current || changingSettings.current || snapshot.phase !== 'idle'
      || snapshot.stable.running || snapshot.lazer.running || snapshot.pluginPresent) return false;
    updating.current = true;
    setUpdatePending(true);
    return true;
  }

  function endUpdate() {
    updating.current = false;
    if (mounted.current) setUpdatePending(false);
  }

  return (
    <main className="launcher-shell">
      <header className="launcher-header">
        <div><h1>Kaleidoscope</h1><p>{snapshot.platform === 'linux' ? 'lazer 連線啟動器。' : 'stable 與 lazer 連線啟動器。'}</p></div>
        <span className="brand-name">Illyaverse</span>
      </header>
      {bridge?.mode === 'preview' && <div className="preview-note"><CircleHelp size={15} aria-hidden="true" />介面預覽 · {bridge.interactive ? '按鈕僅模擬狀態，不會啟動遊戲' : '啟動功能僅在桌面程式提供'}</div>}
      <div className="launcher-panel">
        <ServerSettings settings={settings} activeEndpoint={snapshot.activeEndpoint} interactive={Boolean(bridge?.interactive)} busy={settingsPending} operationBusy={busy}
          loadError={settingsError} retry={() => void retrySettings()} save={saveEndpoint} />
        {snapshot.platform !== 'linux' && <ClientRow client="stable" snapshot={snapshot} endpoint={settings?.endpoint ?? null} disabled={blocked} choose={() => void action('launcher_choose_path', 'stable')} run={() => void action('launcher_start', 'stable')} />}
        <ClientRow client="lazer" snapshot={snapshot} endpoint={settings?.endpoint ?? null} disabled={blocked} choose={() => void action('launcher_choose_path', 'lazer')} run={() => void action('launcher_start', 'lazer')} />
        <Updates bridge={bridge} installBlocked={busy || settingsPending || running || snapshot.pluginPresent || snapshot.phase !== 'idle'} beginInstall={beginUpdate} endInstall={endUpdate} />
      </div>
      <section className={`lifecycle ${error ? 'has-error' : ''}`} aria-label="啟動與清理狀態">
        <div className="lifecycle-main">
          <div className="state-symbol">{busy ? <LoaderCircle className="spin" size={19} aria-hidden="true" /> : error ? <TriangleAlert size={19} aria-hidden="true" /> : <Check size={19} aria-hidden="true" />}</div>
          <div className="lifecycle-copy" aria-live="polite" aria-atomic="true"><strong>{updatePending ? '正在更新' : labels[snapshot.phase]}</strong><p>{updatePending ? '請等待更新驗證與安裝完成；啟動器會重新啟動。' : snapshot.detail}</p></div>
          {cleanupFailed || (snapshot.pluginPresent && !snapshot.lazer.running) ? (
            <Button variant="outline" disabled={!bridge?.interactive || busy || settingsPending} onClick={() => void action('launcher_recover')}><RotateCcw size={14} aria-hidden="true" />重試清理</Button>
          ) : running && <Button variant="quiet" disabled={!bridge?.interactive || busy || settingsPending} onClick={() => void action('launcher_stop')}><Square size={13} aria-hidden="true" />關閉遊戲</Button>}
        </div>
        <p className="cleanup-status" data-testid="cleanup-status">{cleanupTitle}</p>
      </section>
      <footer className="launcher-footer">lazer 關閉後會清理本工具放入的 ruleset；無法連線時不會切回官方。</footer>
    </main>
  );
}
