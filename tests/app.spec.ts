import { test, expect } from '@playwright/test';

test.describe('StageBadger Studio Behavioral User Flows', () => {

  test('should support interactive layout navigation, gallery changes, and broadcast manipulation', async ({ page }) => {
    // Navigate to local Vite dev server
    await page.goto('/');

    // 1. Validate the default setup is READY
    const badge = page.locator('.badge');
    await expect(badge).toHaveText('READY');

    // 2. Validate interactions with Stream Key (Text Entry)
    const urlInput = page.locator('label', { hasText: 'RTMP Server' }).locator('..').locator('input');
    await expect(urlInput).toHaveValue('rtmp://a.rtmp.youtube.com/live2/');

    const streamKeyInput = page.locator('label', { hasText: 'Stream Key' }).locator('..').locator('input');
    await expect(streamKeyInput).toBeEmpty();
    await streamKeyInput.fill('abcd-1234-wqey-9999');
    await expect(streamKeyInput).toHaveValue('abcd-1234-wqey-9999');

    // 3. Interactive Gallery Click Tracking
    // We click the Gaming overlay thumb
    const gamingThumb = page.locator('.gallery-item', { hasText: 'Gaming' });
    await gamingThumb.click();
    await expect(gamingThumb).toHaveClass(/active/);

    // Verify it changed the primary source in the React preview natively
    const cssOverlay = page.locator('#css-overlay-preview');
    await expect(cssOverlay).toBeVisible();
    await expect(cssOverlay).toHaveAttribute('src', /gaming_stream_overlay/);

    // 4. Click Start Broadcast without Tauri Backend (Expect local test error catching)
    // Note: Since Tauri `invoke` is mocked or absent in raw Vite context, pushing standard UI buttons simulates the exception hooks gracefully.
    
    // We set up a dialog handler because our application emits `alert()` upon Tauri failure
    let dialogTriggered = false;
    page.once('dialog', dialog => {
      expect(dialog.message()).toContain('Failed to start stream: TypeError');
      dialog.accept();
      dialogTriggered = true;
    });

    const startBtn = page.locator('button', { hasText: 'Start Broadcast' });
    await expect(startBtn).toBeEnabled();
    await startBtn.click();
    
    // We let the dialog cycle
    await page.waitForTimeout(500);
    expect(dialogTriggered).toBeTruthy();
    
    // Assert it snaps back to READY when failing
    await expect(badge).toHaveText('READY');
  });

});
