export type Client = 'stable' | 'lazer';
export type Phase = 'idle' | 'preparing' | 'running' | 'closing' | 'cleaning' | 'cleanup_failed' | 'error';
export type Compatibility = 'unknown' | 'supported' | 'unsupported';

export interface LauncherSnapshot {
  stable: { path: string | null; running: boolean };
  lazer: { path: string | null; running: boolean; compatibility: Compatibility };
  phase: Phase;
  detail: string;
  pluginPresent: boolean;
  activeEndpoint: string | null;
}

export interface LauncherSettings { endpoint: string }
export const defaultEndpoint = 'https://shdcloud.top';

export type UpdateComponent = 'launcher' | 'bridge';
export interface UpdateStatus {
  configured: boolean;
  repository: string | null;
  currentVersion: string;
  availableVersion: string | null;
  downloadBytes: number | null;
  detail: string;
  bridgeConfigured: boolean;
  bridgeRepository: string | null;
  bridgeVersion: string | null;
  bridgeAvailableVersion: string | null;
  bridgeDownloadBytes: number | null;
  bridgeDetail: string;
}

export type LauncherCommand = 'launcher_snapshot' | 'launcher_choose_path' | 'launcher_start' | 'launcher_stop' | 'launcher_recover';

export interface LauncherBridge {
  mode: 'native' | 'preview';
  interactive: boolean;
  call(command: LauncherCommand, args?: { client: Client }): Promise<LauncherSnapshot>;
  settings(): Promise<LauncherSettings>;
  saveEndpoint(endpoint: string): Promise<LauncherSettings>;
  ready(): Promise<void>;
  updateStatus(): Promise<UpdateStatus>;
  checkUpdates(): Promise<UpdateStatus>;
  installUpdate(component: UpdateComponent): Promise<void>;
}

const phases: Phase[] = ['idle', 'preparing', 'running', 'closing', 'cleaning', 'cleanup_failed', 'error'];
const compatibility: Compatibility[] = ['unknown', 'supported', 'unsupported'];

export const emptySnapshot: LauncherSnapshot = {
  stable: { path: null, running: false },
  lazer: { path: null, running: false, compatibility: 'unknown' },
  phase: 'idle', detail: '正在確認遊戲安裝位置。', pluginPresent: false, activeEndpoint: null,
};

// These checks give immediate form feedback; native validation remains authoritative.
export function validateEndpoint(value: string): string | null {
  if (!value || value.length > 2048 || /[\s\\\u0000-\u001f\u007f]/u.test(value)) return '請輸入完整的 HTTPS 伺服器網址，不要包含空白。';
  try {
    const url = new URL(value);
    if (!/^https:\/\//iu.test(value) || url.protocol !== 'https:' || !url.hostname) return '伺服器網址必須使用 https://。';
    if (url.username || url.password || value.includes('?') || value.includes('#') || !['', '/'].includes(url.pathname)) return '請只填伺服器的基底網址，不要包含帳密、路徑、查詢或 #。';
    const host = url.hostname.toLowerCase().replace(/\.$/u, '');
    if (!host.includes('.') || /^[\d.]+$/u.test(host) || host.includes(':')) return '請使用完整的伺服器網域，例如 shdcloud.top。';
    if (host === 'ppy.sh' || host.endsWith('.ppy.sh')) return '請填 Illyaverse 伺服器的網址，不能使用官方伺服器。';
    return null;
  } catch {
    return '網址格式不正確，例如 https://shdcloud.top。';
  }
}

export function parseSettings(value: unknown): LauncherSettings {
  if (!isRecord(value) || typeof value.endpoint !== 'string' || validateEndpoint(value.endpoint)) {
    throw new Error('無法讀取伺服器設定。請重試讀取。');
  }
  return { endpoint: value.endpoint };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function boundedString(value: unknown, maximum: number): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= maximum;
}

function nullableVersion(value: unknown): boolean {
  return value === null || boundedString(value, 128);
}

function nullableBytes(value: unknown): boolean {
  return value === null || (typeof value === 'number' && Number.isSafeInteger(value) && value > 0);
}

function validFeed(configured: unknown, repository: unknown, version: unknown, bytes: unknown): boolean {
  if (typeof configured !== 'boolean' || !nullableVersion(version) || !nullableBytes(bytes)) return false;
  if (!configured) return repository === null && version === null && bytes === null;
  return boundedString(repository, 140) && /^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_.-]+$/u.test(repository);
}

export function parseUpdateStatus(value: unknown): UpdateStatus {
  if (!isRecord(value) || !boundedString(value.currentVersion, 128) || !nullableVersion(value.bridgeVersion)
    || !boundedString(value.detail, 1000) || !boundedString(value.bridgeDetail, 1000)
    || !validFeed(value.configured, value.repository, value.availableVersion, value.downloadBytes)
    || !validFeed(value.bridgeConfigured, value.bridgeRepository, value.bridgeAvailableVersion, value.bridgeDownloadBytes)) {
    throw new Error('無法讀取更新資訊。請重試讀取。');
  }
  return value as unknown as UpdateStatus;
}

const previewUpdates: UpdateStatus = {
  configured: false, repository: null, currentVersion: '介面預覽', availableVersion: null,
  downloadBytes: null, detail: '更新檢查與安裝僅在桌面程式提供。',
  bridgeConfigured: false, bridgeRepository: null, bridgeVersion: null,
  bridgeAvailableVersion: null, bridgeDownloadBytes: null,
  bridgeDetail: '預覽不會下載或更換連線模組。',
};

function clientState(value: unknown): value is LauncherSnapshot['stable'] {
  return isRecord(value) && (typeof value.path === 'string' || value.path === null) && typeof value.running === 'boolean';
}

function lazerState(value: unknown): value is LauncherSnapshot['lazer'] {
  return isRecord(value) && compatibility.includes(value.compatibility as Compatibility) && clientState(value);
}

export function parseSnapshot(value: unknown): LauncherSnapshot {
  if (!isRecord(value) || !clientState(value.stable) || !lazerState(value.lazer)
    || !phases.includes(value.phase as Phase) || typeof value.detail !== 'string'
    || typeof value.pluginPresent !== 'boolean'
    || !(value.activeEndpoint === null || (typeof value.activeEndpoint === 'string' && !validateEndpoint(value.activeEndpoint)))) {
    throw new Error('啟動器回傳的狀態無法讀取。請重新開啟啟動器。');
  }
  return value as unknown as LauncherSnapshot;
}

// This fixture never reads the filesystem, starts a process, or sends a request.
// Its actions are enabled only by the development build; production browser views are read-only.
function previewBridge(): LauncherBridge {
  const interactive = process.env.NODE_ENV === 'development';
  let snapshot = structuredClone(emptySnapshot);
  snapshot.detail = '選擇遊戲的安裝位置，即可準備啟動（預覽狀態）。';
  let settings = { endpoint: defaultEndpoint };
  return {
    mode: 'preview', interactive,
    async settings() { return structuredClone(settings); },
    async saveEndpoint(endpoint) {
      if (!interactive) throw new Error('請使用桌面版 Kaleidoscope 儲存設定。');
      const error = validateEndpoint(endpoint);
      if (error) throw new Error(error);
      settings = { endpoint: new URL(endpoint).origin };
      return structuredClone(settings);
    },
    async updateStatus() { return structuredClone(previewUpdates); },
    async ready() {},
    async checkUpdates() { return structuredClone(previewUpdates); },
    async installUpdate() { throw new Error('請使用桌面版 Kaleidoscope 安裝更新。'); },
    async call(command, args) {
      if (command === 'launcher_snapshot') return structuredClone(snapshot);
      if (!interactive) throw new Error('請使用桌面版 Kaleidoscope 啟動遊戲。');
      if (command === 'launcher_choose_path' && args) {
        if (args.client === 'stable') snapshot.stable.path = 'C:\\Games\\osu!\\osu!.exe';
        else {
          snapshot.lazer.path = 'C:\\Users\\Player\\AppData\\Local\\osulazer\\current\\osu!.exe';
          snapshot.lazer.compatibility = 'supported';
        }
      } else if (command === 'launcher_start' && args) {
        await new Promise((resolve) => setTimeout(resolve, 350));
        snapshot[args.client].running = true;
        snapshot.phase = 'running';
        snapshot.detail = `${args.client} 已啟動（預覽狀態）。`;
        snapshot.pluginPresent = snapshot.lazer.running;
        snapshot.activeEndpoint = settings.endpoint;
      } else if (command === 'launcher_stop' || command === 'launcher_recover') {
        await new Promise((resolve) => setTimeout(resolve, 350));
        snapshot = {
          ...snapshot,
          stable: { ...snapshot.stable, running: false },
          lazer: { ...snapshot.lazer, running: false },
          phase: 'idle', detail: '遊戲已關閉，連線 ruleset 已清理（預覽狀態）。', pluginPresent: false, activeEndpoint: null,
        };
      }
      return structuredClone(snapshot);
    },
  };
}

export async function createLauncherBridge(): Promise<LauncherBridge> {
  const { isTauri, invoke } = await import('@tauri-apps/api/core');
  if (!isTauri()) return previewBridge();
  return {
    mode: 'native', interactive: true,
    async settings() { return parseSettings(await invoke<unknown>('launcher_settings')); },
    async saveEndpoint(endpoint) { return parseSettings(await invoke<unknown>('launcher_set_endpoint', { endpoint })); },
    async ready() { await invoke<unknown>('launcher_update_ready'); },
    async updateStatus() { return parseUpdateStatus(await invoke<unknown>('launcher_update_status')); },
    async checkUpdates() { return parseUpdateStatus(await invoke<unknown>('launcher_update_check')); },
    async installUpdate(component) { await invoke<unknown>('launcher_update_install', { component }); },
    async call(command, args) {
      return parseSnapshot(await invoke<unknown>(command, args));
    },
  };
}
