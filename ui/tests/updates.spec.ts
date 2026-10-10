import { test, expect, type Page } from '@playwright/test';
import { join } from 'node:path';
import { parseUpdateStatus, type LauncherSnapshot, type UpdateStatus } from '../src/lib/runtime';

const ready: LauncherSnapshot = {
  stable: { path: 'C:\\Games\\osu!\\osu!.exe', running: false },
  lazer: { path: 'C:\\Users\\Player\\AppData\\Local\\osulazer\\current\\osu!.exe', running: false, compatibility: 'supported' },
  phase: 'idle', detail: '遊戲位置已確認，可以啟動。', pluginPresent: false, activeEndpoint: null,
};
const configured: UpdateStatus = {
  configured: true, repository: 'TestPublisher/Kaleidoscope', currentVersion: '0.1.0',
  availableVersion: null, downloadBytes: null, detail: '尚未檢查更新。',
  bridgeConfigured: true, bridgeRepository: 'TestPublisher/Transfiguration', bridgeVersion: '0.1.0',
  bridgeAvailableVersion: null, bridgeDownloadBytes: null, bridgeDetail: '尚未檢查更新。',
};
const available: UpdateStatus = {
  ...configured, availableVersion: '0.2.0', downloadBytes: 12 * 1024 * 1024, detail: '有新版本。',
  bridgeAvailableVersion: '0.1.1', bridgeDownloadBytes: 2 * 1024 * 1024, bridgeDetail: '有新版本。',
};

type Command = { command: string; args?: { component?: string; client?: string; endpoint?: string } };
async function mockNative(page: Page, options: {
  status?: UpdateStatus; checked?: UpdateStatus; snapshot?: LauncherSnapshot;
  checkFailures?: number; malformedStatus?: boolean; installFailure?: string;
  initialSnapshotFailure?: boolean;
} = {}) {
  await page.addInitScript(({ initial, options, fallback, result }) => {
    const commands: Command[] = [];
    const readyObservations: { headingVisible: boolean; stablePathVisible: boolean }[] = [];
    const snapshot = structuredClone(initial);
    let failures = options.checkFailures ?? 0;
    let settings = { endpoint: 'https://shdcloud.top' };
    Object.assign(window, {
      isTauri: true,
      __updateCommands: commands,
      __updateReadyObservations: readyObservations,
      __TAURI_INTERNALS__: {
        async invoke(command: string, args?: Command['args']) {
          commands.push({ command, args });
          if (command === 'launcher_snapshot' && options.initialSnapshotFailure) throw new Error('Synthetic initial snapshot failure');
          if (command === 'launcher_update_ready') {
            const heading = document.querySelector('h1');
            const path = document.querySelector<HTMLInputElement>('#stable-path');
            readyObservations.push({ headingVisible: Boolean(heading?.getClientRects().length && heading.textContent === 'Kaleidoscope'), stablePathVisible: path?.value === initial.stable.path });
            return undefined;
          }
          if (command === 'launcher_settings') return structuredClone(settings);
          if (command === 'launcher_set_endpoint' && args?.endpoint) {
            settings = { endpoint: args.endpoint };
            return structuredClone(settings);
          }
          if (command === 'launcher_update_status') return options.malformedStatus ? structuredClone(snapshot) : structuredClone(options.status ?? fallback);
          if (command === 'launcher_update_check') {
            await new Promise((resolve) => setTimeout(resolve, 160));
            if (failures-- > 0) throw '請稍後重試檢查更新。';
            return structuredClone(options.checked ?? result);
          }
          if (command === 'launcher_update_install') {
            await new Promise((resolve) => setTimeout(resolve, 220));
            if (options.installFailure) throw options.installFailure;
            return null;
          }
          if (command === 'launcher_start' && args?.client) {
            const client = args.client as 'stable' | 'lazer';
            snapshot[client].running = true;
            snapshot.phase = 'running';
            snapshot.pluginPresent = snapshot.lazer.running;
          }
          return structuredClone(snapshot);
        },
      },
    });
  }, { initial: options.snapshot ?? ready, options, fallback: configured, result: available });
}

async function commands(page: Page): Promise<Command[]> {
  return page.evaluate(() => (window as unknown as { __updateCommands: Command[] }).__updateCommands);
}

async function capture(page: Page, name: string) {
  await page.evaluate(() => window.scrollTo(0, 0));
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (process.env.ILLYA_LAUNCHER_CAPTURE_DIR) {
    await page.screenshot({ path: join(process.env.ILLYA_LAUNCHER_CAPTURE_DIR, `${name}.png`), fullPage: true, animations: 'disabled' });
  }
}

test.beforeEach(async ({ page }) => {
  await page.route('**/*', async (route) => {
    const url = new URL(route.request().url());
    const base = new URL(process.env.ILLYA_LAUNCHER_URL ?? 'http://localhost:3010');
    if (url.origin === base.origin) await route.continue();
    else await route.abort('blockedbyclient');
  });
});

test('update status requires independent trusted feeds and bounded native values', () => {
  expect(parseUpdateStatus(available)).toEqual(available);
  expect(parseUpdateStatus({ ...configured, configured: false, repository: null })).toEqual({ ...configured, configured: false, repository: null });
  for (const invalid of [ready, { ...configured, repository: 'https://github.com/TestPublisher/Kaleidoscope' },
    { ...available, bridgeDownloadBytes: -1 }, { ...available, downloadBytes: Number.MAX_SAFE_INTEGER + 1 },
    { ...available, bridgeConfigured: false }, { ...configured, detail: 'x'.repeat(1001) }]) {
    expect(() => parseUpdateStatus(invalid)).toThrow('無法讀取更新資訊。請重試讀取。');
  }
});

test('browser preview states updates are native only without a download action', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: '程式更新' })).toBeVisible();
  await expect(page.getByText('更新檢查與安裝僅在桌面程式提供。')).toBeVisible();
  await expect(page.getByRole('button', { name: '檢查更新', exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: '更新 Kaleidoscope', exact: true })).toHaveCount(0);
});

test('trusted feeds automatically check once and stay compact on desktop and narrow keyboard views', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await mockNative(page);
  await page.setViewportSize({ width: 720, height: 760 });
  await page.goto('/');
  await expect(page.getByRole('button', { name: '更新 Kaleidoscope', exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: '更新 Transfiguration', exact: true })).toBeEnabled();
  await expect(page.getByText('GitHub · TestPublisher/Kaleidoscope', { exact: true })).toBeVisible();
  await expect(page.getByText('GitHub · TestPublisher/Transfiguration', { exact: true })).toBeVisible();
  await expect(page.getByText('可更新至 v0.2.0 · 12 MB', { exact: true })).toBeVisible();
  await capture(page, 'updates-desktop');
  await page.setViewportSize({ width: 360, height: 840 });
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await capture(page, 'updates-narrow');
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_check')).toHaveLength(1);
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_install')).toHaveLength(0);
  const retry = page.getByRole('button', { name: '檢查更新', exact: true });
  await retry.focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('button', { name: '正在檢查', exact: true })).toBeDisabled();
  await expect(retry).toBeEnabled();
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_check')).toHaveLength(2);
  expect(errors).toEqual([]);
});

test('unconfigured native feeds do not send a GitHub request', async ({ page }) => {
  const status: UpdateStatus = { ...configured, configured: false, repository: null, detail: '此版本尚未設定 GitHub 更新來源',
    bridgeConfigured: false, bridgeRepository: null, bridgeVersion: null, bridgeDetail: '此版本尚未設定連線模組更新來源' };
  await mockNative(page, { status });
  await page.goto('/');
  await expect(page.getByText(status.detail, { exact: true })).toBeVisible();
  await expect(page.getByText(status.bridgeDetail, { exact: true })).toBeVisible();
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_check')).toHaveLength(0);
  await expect(page.getByRole('button', { name: '檢查更新', exact: true })).toHaveCount(0);
});

test('native readiness is acknowledged only after the validated launcher interface commits', async ({ page }) => {
  await mockNative(page);
  await page.goto('/');
  await expect.poll(async () => (await commands(page)).filter(({ command }) => command === 'launcher_update_ready').length).toBe(1);
  const recorded = await commands(page);
  expect(recorded.findIndex(({ command }) => command === 'launcher_update_ready')).toBeGreaterThan(recorded.findIndex(({ command }) => command === 'launcher_snapshot'));
  expect(await page.evaluate(() => (window as unknown as { __updateReadyObservations: { headingVisible: boolean; stablePathVisible: boolean }[] }).__updateReadyObservations)).toEqual([{ headingVisible: true, stablePathVisible: true }]);
  const failed = await page.context().newPage();
  await mockNative(failed, { initialSnapshotFailure: true });
  await failed.goto('/');
  await expect(failed.getByText('無法連接啟動器。請重新開啟桌面程式。', { exact: true })).toBeVisible();
  expect((await commands(failed)).filter(({ command }) => command === 'launcher_update_ready')).toHaveLength(0);
  await failed.close();
});

test('native check errors retain launch controls and expose a manual retry', async ({ page }) => {
  await mockNative(page, { checkFailures: 1 });
  await page.goto('/');
  await expect(page.getByText('請稍後重試檢查更新。', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '重試檢查', exact: true }).click();
  await expect(page.getByRole('button', { name: '更新 Transfiguration', exact: true })).toBeEnabled();
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_check')).toHaveLength(2);
});

test('malformed update status cannot disable valid game controls or trigger automatic network checks', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await mockNative(page, { malformedStatus: true });
  await page.goto('/');
  await expect(page.getByText('無法讀取更新資訊。請重試讀取。', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '重新讀取更新', exact: true }).click();
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_status')).toHaveLength(2);
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_check')).toHaveLength(0);
  expect(errors).toEqual([]);
});

test('bridge installation requires an explicit action and locks game and settings operations until native restart', async ({ page }) => {
  await mockNative(page);
  await page.goto('/');
  await expect(page.getByRole('button', { name: '更新 Transfiguration', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '設定', exact: true }).click();
  await page.getByRole('button', { name: '更新 Transfiguration', exact: true }).click();
  await expect(page.getByText('正在驗證並安裝更新，完成後會重新啟動啟動器。', { exact: true })).toBeVisible();
  await expect(page.locator('.lifecycle-copy')).toHaveText('正在更新請等待更新驗證與安裝完成；啟動器會重新啟動。');
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '選擇 lazer 安裝位置' })).toBeDisabled();
  await expect(page.getByRole('button', { name: '設定', exact: true })).toBeDisabled();
  await expect(page.getByRole('textbox', { name: '伺服器網址' })).toBeDisabled();
  await expect(page.getByRole('button', { name: '儲存', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '更新 Kaleidoscope', exact: true })).toBeDisabled();
  await capture(page, 'updates-installing');
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_install')).toEqual([{ command: 'launcher_update_install', args: { component: 'bridge' } }]);
  expect((await commands(page)).filter(({ command }) => command === 'launcher_start' || command === 'launcher_set_endpoint')).toHaveLength(0);
});

test('installation failure unlocks controls and preserves the native error', async ({ page }) => {
  await mockNative(page, { installFailure: '更新簽章驗證失敗；未安裝任何檔案' });
  await page.goto('/');
  await expect(page.getByRole('button', { name: '更新 Kaleidoscope', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '更新 Kaleidoscope', exact: true }).click();
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeDisabled();
  await expect(page.getByText('更新簽章驗證失敗；未安裝任何檔案', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: '更新 Kaleidoscope', exact: true })).toBeEnabled();
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_install')).toEqual([{ command: 'launcher_update_install', args: { component: 'launcher' } }]);
});

test('a running game or retained connection module blocks both installers', async ({ page }) => {
  await mockNative(page, { snapshot: { ...ready, lazer: { ...ready.lazer, running: true }, phase: 'running', pluginPresent: true } });
  await page.goto('/');
  await expect(page.getByRole('button', { name: '檢查更新', exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: '更新 Kaleidoscope', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '更新 Transfiguration', exact: true })).toBeDisabled();
  await expect(page.getByText('請先完成目前操作、關閉遊戲並清理連線模組，再安裝更新。', { exact: true })).toBeVisible();
  expect((await commands(page)).filter(({ command }) => command === 'launcher_update_install')).toHaveLength(0);
});
