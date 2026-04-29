import { test, expect } from '@playwright/test';

test.describe('StageBadger Studio React UI', () => {
  test('should render the 3-pane layout securely without crashing', async ({ page }) => {
    // Navigate to local Vite dev server
    await page.goto('/');

    // Validate the Sidebar Header
    const header = page.locator('h1', { hasText: 'StageBadger' });
    await expect(header).toBeVisible();

    // Validate Status Badge reads READY indicating React parsed state correctly
    const badge = page.locator('.badge');
    await expect(badge).toHaveText('READY');

    // Validate AV Form Rendering
    await expect(page.locator('label', { hasText: 'Camera Source' })).toBeVisible();
    await expect(page.locator('label', { hasText: 'Microphone Source' })).toBeVisible();
    
    // Validate Overlay Gallery mapped items correctly natively in React
    const cyberpunkOverlay = page.locator('.gallery-item .title', { hasText: 'Cyberpunk' });
    await expect(cyberpunkOverlay).toBeVisible();
    
    // Validate the split panels
    const videoCameraCanvas = page.locator('#camera-preview');
    await expect(videoCameraCanvas).toBeVisible();

    const terminalHeader = page.locator('.terminal-header h3');
    await expect(terminalHeader).toHaveText('🔴 Native Whisper Token Stream (MKL/Metal)');
  });
});
