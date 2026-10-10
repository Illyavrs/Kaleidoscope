import { test, expect, type Page } from '@playwright/test';
import { join } from 'node:path';
import type { LauncherSnapshot } from '../src/lib/runtime';

const ready: LauncherSnapshot = {
  stable: { path: 'C:\\Games\\osu!\\osu!.exe', running: false },
  lazer: { path: 'C:\\Users\\Player\\AppData\\Local\\osulazer\\current\\osu!.exe', running: false, compatibility: 'supported' },
  phase: 'idle', detail: '遊戲位置已確認，可以啟動。', pluginPresent: false, activeEndpoint: null,
};

async function mockNative(page: Page, state: LauncherSnapshot, options?: { endpoint?: string; failReads?: number; saveError?: string; readDelay?: number; pollDelay?: number }) {
  await page.addInitScript(({ initial, options }) => {
    let snapshot = structuredClone(initial);
    let settings = { endpoint: options?.endpoint ?? 'https://shdcloud.top' };
    let readFailures = options?.failReads ?? 0;
    let saveError = options?.saveError;
    const polls = { count: 0, active: 0, max: 0 };
    const commands: { command: string; args?: { client?: 'stable' | 'lazer'; endpoint?: string } }[] = [];
    Object.assign(window, {
      isTauri: true,
      __launcherTestCommands: commands,
      __launcherTestPolls: polls,
      __TAURI_INTERNALS__: {
        async invoke(command: string, args?: { client?: 'stable' | 'lazer'; endpoint?: string }) {
          commands.push({ command, args });
          if (command === 'launcher_snapshot' && polls.count++ > 0 && options?.pollDelay) {
            polls.active += 1;
            polls.max = Math.max(polls.max, polls.active);
            try { await new Promise((resolve) => setTimeout(resolve, options.pollDelay)); }
            finally { polls.active -= 1; }
          }
          if (command === 'launcher_settings') {
            if (options?.readDelay) await new Promise((resolve) => setTimeout(resolve, options.readDelay));
            if (readFailures-- > 0) throw new Error('Synthetic settings read failure');
            return structuredClone(settings);
          }
          if (command === 'launcher_set_endpoint' && args?.endpoint) {
            await new Promise((resolve) => setTimeout(resolve, 250));
            if (saveError) { const failure = saveError; saveError = undefined; throw new Error(failure); }
            settings = { endpoint: new URL(args.endpoint).origin };
            return structuredClone(settings);
          }
          if (command === 'launcher_start' && args?.client) {
            await new Promise((resolve) => setTimeout(resolve, 120));
            snapshot[args.client].running = true;
            snapshot.phase = 'running';
            snapshot.pluginPresent = snapshot.lazer.running;
            snapshot.detail = `${args.client} 已啟動。`;
            snapshot.activeEndpoint = settings.endpoint;
          } else if (command === 'launcher_stop' || command === 'launcher_recover') {
            await new Promise((resolve) => setTimeout(resolve, 120));
            snapshot = { ...snapshot, stable: { ...snapshot.stable, running: false }, lazer: { ...snapshot.lazer, running: false }, phase: 'idle', detail: '遊戲已關閉，連線 ruleset 已清理。', pluginPresent: false, activeEndpoint: null };
          }
          return structuredClone(snapshot);
        },
      },
    });
  }, { initial: state, options });
}

async function capture(page: Page, name: string) {
  await page.evaluate(() => window.scrollTo(0, 0));
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  expect(await page.locator('body').evaluate((node) => getComputedStyle(node).backgroundColor)).toBe('rgb(0, 0, 0)');
  if (process.env.ILLYA_LAUNCHER_CAPTURE_DIR) {
    await page.screenshot({ path: join(process.env.ILLYA_LAUNCHER_CAPTURE_DIR, `${name}.png`), fullPage: true, animations: 'disabled' });
  }
}

test('development browser preview labels its fixture and simulates choosing, running and cleanup', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await page.setViewportSize({ width: 720, height: 640 });
  await page.goto('/');
  await expect(page.getByText('介面預覽 · 按鈕僅模擬狀態，不會啟動遊戲')).toBeVisible();
  await expect(page.getByRole('button', { name: '啟動 lazer', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '選擇 lazer 安裝位置' }).click();
  await expect(page.getByRole('textbox', { name: 'lazer 安裝位置' })).toHaveValue(/osulazer/);
  await page.getByRole('button', { name: '啟動 lazer', exact: true }).click();
  await expect(page.getByRole('button', { name: '關閉並重新啟動 lazer' })).toBeEnabled();
  await expect(page.getByTestId('cleanup-status')).toHaveText('連線 ruleset 使用中');
  await page.getByRole('button', { name: '關閉遊戲', exact: true }).click();
  await expect(page.getByTestId('cleanup-status')).toHaveText('沒有暫存的連線 ruleset');
  expect(errors).toEqual([]);
});

test('native bridge shows ready and running states with a configured server and keyboard controls', async ({ page }) => {
  await mockNative(page, ready);
  await page.setViewportSize({ width: 720, height: 640 });
  await page.goto('/');
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeEnabled();
  await expect(page.getByText('介面預覽', { exact: false })).toHaveCount(0);
  await expect(page.getByRole('combobox')).toHaveCount(0);
  await capture(page, 'launcher-ready-desktop');
  await page.getByRole('button', { name: '啟動 lazer', exact: true }).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('button', { name: '關閉並重新啟動 lazer' })).toBeEnabled();
  await capture(page, 'launcher-running-desktop');
  await page.setViewportSize({ width: 390, height: 844 });
  await capture(page, 'launcher-running-mobile');
  await page.getByRole('button', { name: '關閉並重新啟動 lazer' }).click();
  await expect(page.getByRole('button', { name: '關閉並重新啟動 lazer' })).toBeEnabled();
  const commands = await page.evaluate(() => (window as unknown as { __launcherTestCommands: { command: string; args?: { client: string } }[] }).__launcherTestCommands);
  expect(commands.filter((entry) => entry.command === 'launcher_start')).toEqual([
    { command: 'launcher_start', args: { client: 'lazer' } }, { command: 'launcher_start', args: { client: 'lazer' } },
  ]);
});

test('native detected stable path can launch directly without opening a chooser', async ({ page }) => {
  const installed = 'C:\\Users\\Player\\AppData\\Local\\osu!\\osu!.exe';
  await mockNative(page, { ...ready, stable: { path: installed, running: false } });
  await page.goto('/');
  await expect(page.getByRole('textbox', { name: 'stable 安裝位置' })).toHaveValue(installed);
  await expect(page.locator('[data-client="stable"]').getByText('變更位置', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '啟動 stable', exact: true }).click();
  await expect(page.getByRole('button', { name: '關閉並重新啟動 stable' })).toBeEnabled();
  const commands = await page.evaluate(() => (window as unknown as { __launcherTestCommands: { command: string; args?: { client: string } }[] }).__launcherTestCommands);
  expect(commands.filter((entry) => entry.command === 'launcher_choose_path')).toEqual([]);
  expect(commands.filter((entry) => entry.command === 'launcher_start')).toEqual([
    { command: 'launcher_start', args: { client: 'stable' } },
  ]);
});

test('missing stable installation requests manual selection and keeps launching disabled', async ({ page }) => {
  await mockNative(page, { ...ready, stable: { path: null, running: false } });
  await page.goto('/');
  await expect(page.getByRole('textbox', { name: 'stable 安裝位置' })).toHaveValue('');
  await expect(page.getByText('未找到預設安裝位置，請選擇 osu!.exe。')).toBeVisible();
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '選擇 stable 安裝位置' })).toBeEnabled();
});

test('unsupported lazer and failed cleanup block launching while cleanup remains recoverable', async ({ page }) => {
  await mockNative(page, { ...ready, phase: 'cleanup_failed', pluginPresent: true, detail: '連線 ruleset 無法刪除。請關閉 lazer 後重試清理。' });
  await page.setViewportSize({ width: 720, height: 640 });
  await page.goto('/');
  await expect(page.getByRole('button', { name: '啟動 lazer', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '重試清理' })).toBeEnabled();
  await capture(page, 'launcher-cleanup-failed-desktop');
  await page.setViewportSize({ width: 390, height: 844 });
  await capture(page, 'launcher-cleanup-failed-mobile');
  await page.getByRole('button', { name: '重試清理' }).click();
  await expect(page.getByTestId('cleanup-status')).toHaveText('沒有暫存的連線 ruleset');
  await expect(page.getByRole('button', { name: '啟動 lazer', exact: true })).toBeEnabled();
  await capture(page, 'launcher-ready-mobile');

  const unsupported = await page.context().newPage();
  await mockNative(unsupported, { ...ready, lazer: { ...ready.lazer, compatibility: 'unsupported' } });
  await unsupported.goto('/');
  await expect(unsupported.getByRole('button', { name: '啟動 lazer', exact: true })).toBeDisabled();
  await expect(unsupported.getByText('此版本暫不支援，請等待啟動器與連線 ruleset 配套更新。')).toBeVisible();
  await unsupported.close();
});

test('server settings validate HTTPS origins before saving and preserve draft after a native save failure', async ({ page }) => {
  await mockNative(page, ready, { saveError: '設定檔無法寫入。請確認權限後重試。' });
  await page.goto('/');
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://shdcloud.top');
  await page.getByRole('button', { name: '設定', exact: true }).click();
  const field = page.getByRole('textbox', { name: '伺服器網址' });
  for (const invalid of ['http://illya.test', 'https://user:secret@example.test', 'https://example.test/api', 'https://example.test?token=fixture', 'https://example.test#fixture', 'https://osu.ppy.sh', 'https://example.test\\other', 'https://exam ple.test', 'https://localhost', 'https://127.0.0.1', 'https://[::1]', 'https://example.test?', 'https://example.test#']) {
    await field.fill(invalid);
    await page.getByRole('button', { name: '儲存', exact: true }).click();
    await expect(field).toHaveAttribute('aria-invalid', 'true');
    await expect(field).toBeFocused();
  }
  const before = await page.evaluate(() => (window as unknown as { __launcherTestCommands: { command: string }[] }).__launcherTestCommands);
  expect(before.filter((entry) => entry.command === 'launcher_set_endpoint')).toEqual([]);
  await field.fill('https://community.example.test:8443/');
  await page.getByRole('button', { name: '儲存', exact: true }).click();
  await expect(page.getByText('設定檔無法寫入。請確認權限後重試。')).toBeVisible();
  await expect(field).toHaveValue('https://community.example.test:8443/');
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://shdcloud.top');
  await page.getByRole('button', { name: '儲存', exact: true }).click();
  await expect(page.getByText('已儲存', { exact: true })).toBeVisible();
  await expect(field).toHaveValue('https://community.example.test:8443');
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://community.example.test:8443');
});

test('saving while lazer runs affects the next launch and reset is an explicit save', async ({ page }) => {
  await mockNative(page, { ...ready, lazer: { ...ready.lazer, running: true }, phase: 'running', pluginPresent: true, activeEndpoint: 'https://old.example.test' }, { endpoint: 'https://old.example.test' });
  await page.goto('/');
  await page.getByRole('button', { name: '設定', exact: true }).click();
  const field = page.getByRole('textbox', { name: '伺服器網址' });
  await field.fill('https://next.example.test');
  await page.getByRole('button', { name: '儲存', exact: true }).click();
  await expect(page.getByRole('button', { name: '正在儲存' })).toBeDisabled();
  await expect(page.getByRole('button', { name: '關閉並重新啟動 lazer' })).toBeDisabled();
  await expect(page.getByRole('button', { name: '關閉遊戲', exact: true })).toBeDisabled();
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://next.example.test');
  await expect(page.getByText('本次遊戲連線：')).toHaveText('本次遊戲連線：https://old.example.test');
  const saved = await page.evaluate(() => (window as unknown as { __launcherTestCommands: { command: string }[] }).__launcherTestCommands);
  expect(saved.filter((entry) => entry.command === 'launcher_start' || entry.command === 'launcher_stop')).toEqual([]);
  expect(saved.filter((entry) => entry.command === 'launcher_set_endpoint')).toHaveLength(1);
  await page.getByRole('button', { name: '還原預設網址' }).click();
  await expect(field).toHaveValue('https://shdcloud.top');
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://next.example.test');
  await page.getByRole('button', { name: '儲存', exact: true }).click();
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://shdcloud.top');
  await page.getByRole('button', { name: '關閉並重新啟動 lazer' }).click();
  await expect(page.getByText('本次遊戲連線：')).toHaveText('本次遊戲連線：https://shdcloud.top');
});

test('settings read failure has an independent retry and does not block stale plugin cleanup', async ({ page }) => {
  await mockNative(page, { ...ready, phase: 'cleanup_failed', pluginPresent: true }, { failReads: 1 });
  await page.goto('/');
  await expect(page.getByText('無法讀取伺服器設定。請重試讀取。')).toBeVisible();
  await expect(page.getByRole('button', { name: '設定', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: '啟動 lazer', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: '重試清理' }).click();
  await expect(page.getByTestId('cleanup-status')).toHaveText('沒有暫存的連線 ruleset');
  await page.getByRole('button', { name: '重試讀取' }).click();
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://shdcloud.top');
  await expect(page.getByRole('button', { name: '啟動 lazer', exact: true })).toBeEnabled();
});

test('settings desktop and narrow views retain readable controls, focus, and no horizontal overflow', async ({ page }) => {
  await mockNative(page, ready, { endpoint: 'https://community.example.test:8443' });
  await page.setViewportSize({ width: 800, height: 720 });
  await page.goto('/');
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://community.example.test:8443');
  await expect(page.getByRole('button', { name: '啟動 stable', exact: true })).toBeDisabled();
  await expect(page.getByText('目前網址使用自訂連接埠；stable 需要 HTTPS 443。')).toBeVisible();
  await page.getByRole('button', { name: '設定', exact: true }).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('button', { name: '設定', exact: true })).toHaveAttribute('aria-expanded', 'true');
  await capture(page, 'launcher-settings-desktop');
  await page.setViewportSize({ width: 390, height: 844 });
  await capture(page, 'launcher-settings-narrow');
  await page.getByRole('textbox', { name: '伺服器網址' }).fill('https://example.test/api');
  await page.getByRole('button', { name: '儲存', exact: true }).click();
  await capture(page, 'launcher-settings-validation-narrow');
});

test('settings cannot open with an empty draft during the initial slow read', async ({ page }) => {
  await mockNative(page, ready, { endpoint: 'https://community.example.test', readDelay: 1200 });
  await page.goto('/');
  await expect(page.getByRole('button', { name: '設定', exact: true })).toBeDisabled();
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://community.example.test');
  await page.getByRole('button', { name: '設定', exact: true }).click();
  await expect(page.getByRole('textbox', { name: '伺服器網址' })).toHaveValue('https://community.example.test');
});

test('slow snapshot polls never overlap and regress the displayed lifecycle', async ({ page }) => {
  await mockNative(page, ready, { pollDelay: 3200 });
  await page.goto('/');
  await expect(page.getByTestId('saved-endpoint')).toHaveText('https://shdcloud.top');
  await expect.poll(async () => page.evaluate(() => (window as unknown as { __launcherTestPolls: { count: number } }).__launcherTestPolls.count), { timeout: 8000 }).toBeGreaterThanOrEqual(3);
  const polls = await page.evaluate(() => (window as unknown as { __launcherTestPolls: { max: number } }).__launcherTestPolls);
  expect(polls.max).toBe(1);
});

test('a malformed native snapshot fails closed without enabling preview actions', async ({ page }) => {
  await page.addInitScript(() => Object.assign(window, {
    isTauri: true, __TAURI_INTERNALS__: { async invoke() { return { phase: 'idle' }; } },
  }));
  await page.goto('/');
  await expect(page.getByText('無法連接啟動器。請重新開啟桌面程式。')).toBeVisible();
  await expect(page.getByRole('button', { name: '選擇 lazer 安裝位置' })).toBeDisabled();
  await expect(page.getByText('介面預覽', { exact: false })).toHaveCount(0);
});
