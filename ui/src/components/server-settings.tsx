'use client';

import { useRef, useState, type FormEvent } from 'react';
import { Check, LoaderCircle, Settings2, ShieldCheck } from 'lucide-react';
import { defaultEndpoint, validateEndpoint, type LauncherSettings } from '@/lib/runtime';
import { Button } from './button';

export function ServerSettings({ settings, activeEndpoint, interactive, busy, operationBusy, loadError, retry, save }: {
  settings: LauncherSettings | null;
  activeEndpoint: string | null;
  interactive: boolean;
  busy: boolean;
  operationBusy: boolean;
  loadError: string | null;
  retry: () => void;
  save: (endpoint: string) => Promise<LauncherSettings>;
}) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const submitting = useRef(false);

  function toggle() {
    if (!open) setDraft(settings?.endpoint ?? '');
    setError(null);
    setSaved(false);
    setOpen(!open);
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!interactive || busy || operationBusy || !settings || submitting.current) return;
    const invalid = validateEndpoint(draft);
    setError(invalid);
    setSaved(false);
    if (invalid) { input.current?.focus(); return; }
    submitting.current = true;
    try {
      const next = await save(draft);
      setDraft(next.endpoint);
      setSaved(true);
    } catch (failure) {
      setError(failure instanceof Error && failure.message.length <= 1000
        ? failure.message : '設定未儲存。請重試。');
    } finally {
      submitting.current = false;
    }
  }

  return (
    <div className="server-settings">
      <div className="server-row">
        <div className="server-label">
          <ShieldCheck size={18} aria-hidden="true" />
          <div><strong>Illyaverse</strong><p data-testid="saved-endpoint">{settings?.endpoint ?? (loadError ? '設定尚未讀取' : '正在讀取伺服器設定…')}</p></div>
        </div>
        <Button variant="quiet" onClick={toggle} disabled={busy || !settings} aria-expanded={open} aria-controls="server-settings-panel">
          <Settings2 size={15} aria-hidden="true" />設定
        </Button>
      </div>
      {loadError && <div className="settings-load-error" role="alert"><p>{loadError}</p><Button variant="outline" onClick={retry} disabled={busy || operationBusy}>重試讀取</Button></div>}
      {open && <form id="server-settings-panel" className="settings-panel" onSubmit={(event) => void submit(event)} noValidate>
        <div className="settings-heading"><h2>伺服器設定</h2><p>儲存後於下次啟動遊戲套用；連線失敗時不會切回官方。</p></div>
        <label htmlFor="server-endpoint">伺服器網址</label>
        <input ref={input} id="server-endpoint" type="url" inputMode="url" autoCapitalize="none" autoComplete="off" spellCheck={false}
          placeholder={defaultEndpoint} maxLength={2048} value={draft} disabled={!interactive || busy || operationBusy || !settings}
          aria-invalid={Boolean(error)} aria-describedby={`endpoint-hint${error ? ' endpoint-error' : ''}`}
          onChange={(event) => { setDraft(event.target.value); setError(null); setSaved(false); }} />
        <p id="endpoint-hint" className="settings-hint">只填 HTTPS 基底網址。lazer 可使用自訂連接埠；stable 使用 HTTPS 443。</p>
        {activeEndpoint && <p className="active-endpoint">本次遊戲連線：<span>{activeEndpoint}</span></p>}
        {error && <p id="endpoint-error" className="settings-error" role="alert">{error}</p>}
        <div className="settings-actions">
          <Button type="button" variant="quiet" disabled={!interactive || busy || operationBusy || !settings} onClick={() => { setDraft(defaultEndpoint); setError(null); setSaved(false); }}>還原預設網址</Button>
          <div>
            {saved && <span className="settings-saved" role="status"><Check size={14} aria-hidden="true" />已儲存</span>}
            <Button type="submit" disabled={!interactive || busy || operationBusy || !settings || draft === settings.endpoint}>
              {busy && <LoaderCircle className="spin" size={14} aria-hidden="true" />}{busy ? '正在儲存' : '儲存'}
            </Button>
          </div>
        </div>
      </form>}
    </div>
  );
}
