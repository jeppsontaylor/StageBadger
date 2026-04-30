import { expect, test } from '@playwright/test';

test.describe('StageBadger studio shell', () => {
  const transcriptFixture = {
    schemaVersion: 1,
    sessionId: 'preview-fixture',
    sourceLabel: 'Preview Mic',
    micId: 'Default Microphone',
    startedAtMs: 0,
    updatedAtMs: 2800,
    finalization: {
      isFinal: false,
      finalizedAtMs: null,
      sourceMediaPath: null,
      finalMediaPath: null,
      sidecarPaths: [],
      audioSource: 'Default Microphone'
    },
    segments: [
      {
        id: 'chunk-000001',
        chunkId: 1,
        startMs: 1200,
        endMs: 2800,
        confidence: 0.93,
        sourceModel: 'base',
        text: 'hello world',
        words: [
          {
            text: 'hello',
            normalizedText: 'hello',
            confidence: 0.96,
            startMs: 1200,
            endMs: 1700,
            sourceModel: 'base',
            chunkId: 1
          },
          {
            text: 'world',
            normalizedText: 'world',
            confidence: 0.58,
            startMs: 1800,
            endMs: 2800,
            sourceModel: 'base',
            chunkId: 1
          }
        ],
        alternates: [
          {
            modelName: 'tiny',
            confidence: 0.74,
            text: 'hallo world',
            words: [
              {
                text: 'hallo',
                normalizedText: 'hallo',
                confidence: 0.73,
                startMs: 1200,
                endMs: 1700,
                sourceModel: 'tiny',
                chunkId: 1
              },
              {
                text: 'world',
                normalizedText: 'world',
                confidence: 0.75,
                startMs: 1800,
                endMs: 2800,
                sourceModel: 'tiny',
                chunkId: 1
              }
            ]
          }
        ]
      }
    ]
  };

  test('supports guided YouTube RTMPS setup, live start, record-only, and overlays', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByRole('heading', { name: 'StageBadger' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Destination', exact: true })).toBeVisible();
    await expect(page.getByText('Setup')).toBeVisible();

    await page.getByRole('button', { name: 'Destination', exact: true }).click();
    await page.getByLabel('Label', { exact: true }).fill('YouTube RTMPS');
    await page.getByLabel('Server URL', { exact: true }).fill('rtmps://a.rtmps.youtube.com/live2/');
    await page.getByLabel('Stream key', { exact: true }).fill('abcd-1234-wqey-9999');
    await page.getByLabel('Privacy note', { exact: true }).fill('Confirm privacy in YouTube Studio');
    await page.getByLabel('I confirmed YouTube Live is enabled', { exact: true }).check();

    await page.getByRole('button', { name: 'Test destination' }).click();
    await expect(page.locator('.destination-test')).toContainText('Ready');
    await expect(page.locator('.destination-test code')).toContainText('[redacted]');

    await page.getByRole('button', { name: 'Save destination' }).click();
    await expect(page.locator('.destination-row')).toContainText('YouTube RTMPS');
    await expect(page.locator('.destination-row')).toContainText('Keychain key saved');

    await page.getByRole('button', { name: 'Overlays', exact: true }).click();
    await page.getByRole('button', { name: 'Gaming' }).click();
    await expect(page.locator('.program-overlay')).toHaveAttribute('src', /gaming\.png/);

    await page.getByRole('button', { name: 'Inputs', exact: true }).click();
    await expect(page.locator('label').filter({ hasText: 'Program feed' }).locator('select')).toBeVisible();
    await expect(page.getByLabel('Program feed')).toContainText('Default Camera');
    await expect(page.getByLabel('Program feed')).toContainText('Capture screen 0');
    await page.getByLabel('Program feed').selectOption('screen-1');
    await expect(page.locator('.preview-stage')).toHaveClass(/screen-primary/);
    await page.getByLabel('In-screen', { exact: true }).check();
    await expect(page.getByLabel('In-screen feed')).toBeVisible();
    await expect(page.locator('.pip-frame.bottomRight')).toBeVisible();
    await expect(page.getByLabel('In-screen feed')).not.toHaveValue('screen-1');

    await page.getByRole('button', { name: 'Video', exact: true }).click();
    await page.getByLabel('Native depth of field', { exact: true }).check();
    await expect(page.getByText('Native DOF requested')).toBeVisible();

    await page.getByRole('button', { name: 'Recording', exact: true }).click();
    await expect(page.getByLabel('Video bitrate', { exact: true })).toBeVisible();

    await page.getByRole('button', { name: 'Go Live' }).click();
    await expect(page.locator('.top-strip strong')).toHaveText('LIVE');
    await expect(page.locator('.top-strip')).toContainText('YouTube RTMPS');
    await expect(page.locator('.inline-error')).toHaveCount(0);
    const liveCommand = await page.evaluate(() => (
      window as Window & { __STAGEBADGER_LAST_COMMAND__?: { command: string; args?: Record<string, unknown> } }
    ).__STAGEBADGER_LAST_COMMAND__);
    expect(liveCommand?.command).toBe('start_live_session');
    expect(JSON.stringify(liveCommand?.args)).toContain('"videoFeeds"');
    await page.getByRole('button', { name: 'Inputs', exact: true }).click();
    await page.getByLabel('Program feed').selectOption('camera-0');
    await expect(page.getByText('Output locked')).toBeVisible();

    await page.getByRole('button', { name: 'Stop' }).click();
    await expect(page.locator('.top-strip strong')).toHaveText('IDLE');

    await page.getByRole('button', { name: 'Record', exact: true }).click();
    await expect(page.locator('.top-strip strong')).toHaveText('RECORDING');
    await expect(page.getByText(/stagebadger-recording\.mp4/)).toBeVisible();
    const recordCommand = await page.evaluate(() => (
      window as Window & { __STAGEBADGER_LAST_COMMAND__?: { command: string; args?: Record<string, unknown> } }
    ).__STAGEBADGER_LAST_COMMAND__);
    expect(recordCommand?.command).toBe('start_recording');
    expect(JSON.stringify(recordCommand?.args)).toContain('"videoFeeds"');
    await page.getByRole('button', { name: 'Stop' }).click();
    await expect(page.locator('.top-strip strong')).toHaveText('IDLE');

    await page.screenshot({ path: test.info().outputPath('studio-shell.png'), fullPage: true });
  });

  test('renders transcript fixtures with timestamps and confidence styling', async ({ page }) => {
    await page.addInitScript((fixture) => {
      window.localStorage.setItem('stagebadger.transcriptFixture', JSON.stringify(fixture));
    }, transcriptFixture);

    await page.goto('/');

    await page.getByRole('button', { name: 'Preview', exact: true }).click();
    await page.getByRole('button', { name: 'Transcript', exact: true }).click();

    await expect(page.locator('.transcript-segment')).toHaveCount(1);
    await expect(page.locator('.transcript-segment-header')).toContainText('00:00:01.200 - 00:00:02.800');
    await expect(page.locator('.segment-model')).toContainText('base');
    await expect(page.locator('.segment-confidence')).toContainText('93%');
    await expect(page.locator('.word-token.high')).toContainText('hello');
    await expect(page.locator('.word-token.low')).toContainText('world');
    await expect(page.locator('.segment-alternate')).toContainText('tiny 74%');
  });

  test('left control rail behaves as a single-open accordion', async ({ page }) => {
    await page.goto('/');

    await expect(page.locator('.accordion-panel')).toHaveCount(0);

    await page.getByRole('button', { name: 'Destination', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Destination', exact: true })).toHaveAttribute('aria-expanded', 'true');
    await expect(page.getByRole('button', { name: 'Audio', exact: true })).toHaveAttribute('aria-expanded', 'false');
    await expect(page.locator('.accordion-panel')).toHaveCount(1);
    await expect(page.getByLabel('Server URL', { exact: true })).toBeVisible();

    await page.getByRole('button', { name: 'Audio', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Destination', exact: true })).toHaveAttribute('aria-expanded', 'false');
    await expect(page.getByRole('button', { name: 'Audio', exact: true })).toHaveAttribute('aria-expanded', 'true');
    await expect(page.locator('.accordion-panel')).toHaveCount(1);
    await expect(page.getByLabel('Microphone level', { exact: true })).toBeVisible();

    await page.getByRole('button', { name: 'Audio', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Audio', exact: true })).toHaveAttribute('aria-expanded', 'false');
    await expect(page.locator('.accordion-panel')).toHaveCount(0);
  });

  for (const viewport of [
    { width: 1280, height: 720 },
    { width: 1440, height: 900 },
    { width: 900, height: 720 }
  ]) {
    test(`has a stable non-overlapping layout at ${viewport.width}x${viewport.height}`, async ({ page }) => {
      await page.setViewportSize(viewport);
      await page.goto('/');
      await expect(page.locator('.preview-stage')).toBeVisible();
      await expect(page.locator('.transport')).toBeVisible();
      await expect(page.locator('.left-rail')).toBeVisible();
      await page.getByRole('button', { name: 'Destination', exact: true }).click();
      const previewBox = await page.locator('.preview-stage').boundingBox();
      const transportBox = await page.locator('.transport').boundingBox();
      const leftRailBox = await page.locator('.left-rail').boundingBox();
      expect(previewBox?.height ?? 0).toBeGreaterThan(240);
      expect(transportBox?.y ?? 0).toBeGreaterThan((previewBox?.y ?? 0) + (previewBox?.height ?? 0) - 2);
      expect(leftRailBox?.width ?? 0).toBeGreaterThan(250);
      expect(previewBox?.x ?? 0).toBeGreaterThanOrEqual((leftRailBox?.x ?? 0) + (leftRailBox?.width ?? 0) - 1);
    });
  }
});
