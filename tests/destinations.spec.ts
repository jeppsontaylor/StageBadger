import { expect, test } from '@playwright/test';
import {
  YOUTUBE_RTMPS_SERVER,
  normalizeRtmpServerUrl,
  parseClipboardDestination,
  redactedDestinationUrl
} from '../src/destinations';

test.describe('RTMPS helpers', () => {
  test('normalizes server URLs and rejects invalid schemes', () => {
    expect(normalizeRtmpServerUrl(' rtmps://a.rtmps.youtube.com/live2 ')).toBe(YOUTUBE_RTMPS_SERVER);
    expect(() => normalizeRtmpServerUrl('https://example.com/live')).toThrow(/rtmp:\/\/ or rtmps:\/\//i);
  });

  test('parses clipboard payloads for raw keys and full URLs', () => {
    expect(parseClipboardDestination('abcd-1234-xyz')).toEqual({
      serverUrl: YOUTUBE_RTMPS_SERVER,
      streamKey: 'abcd-1234-xyz'
    });

    expect(parseClipboardDestination('  rtmps://example.com/live2/my-key  ')).toEqual({
      serverUrl: 'rtmps://example.com/live2/',
      streamKey: 'my-key'
    });
  });

  test('redacts stream keys in URLs', () => {
    expect(redactedDestinationUrl(YOUTUBE_RTMPS_SERVER, 'super-secret-key')).toContain('[redacted]');
    expect(redactedDestinationUrl(YOUTUBE_RTMPS_SERVER, 'super-secret-key')).not.toContain('super-secret-key');
  });
});
