export const YOUTUBE_RTMPS_SERVER = 'rtmps://a.rtmps.youtube.com/live2/';

export type DestinationProvider = 'youtube' | 'custom';

export interface ParsedClipboardDestination {
  serverUrl: string;
  streamKey: string;
}

export function normalizeRtmpServerUrl(value: string) {
  const trimmed = value.trim();
  if (!trimmed) {
    throw new Error('Enter an RTMP or RTMPS server URL.');
  }

  const lower = trimmed.toLowerCase();
  if (!lower.startsWith('rtmp://') && !lower.startsWith('rtmps://')) {
    throw new Error('Server URL must start with rtmp:// or rtmps://.');
  }

  return trimmed.endsWith('/') ? trimmed : `${trimmed}/`;
}

export function parseClipboardDestination(value: string, fallbackServerUrl = YOUTUBE_RTMPS_SERVER): ParsedClipboardDestination {
  const text = value.trim();
  if (!text) {
    throw new Error('Clipboard does not contain a stream key or RTMPS URL.');
  }

  const compact = text.replace(/\s+/g, '');
  if (/^rtmps?:\/\//i.test(compact)) {
    const slashIndex = compact.lastIndexOf('/');
    if (slashIndex <= 'rtmp://'.length) {
      throw new Error('Clipboard RTMP URL is missing a stream key.');
    }

    const serverUrl = normalizeRtmpServerUrl(compact.slice(0, slashIndex + 1));
    const streamKey = compact.slice(slashIndex + 1).trim();
    if (!streamKey) {
      throw new Error('Clipboard RTMP URL is missing a stream key.');
    }

    return { serverUrl, streamKey };
  }

  return {
    serverUrl: normalizeRtmpServerUrl(fallbackServerUrl),
    streamKey: text
  };
}

export function redactStreamKey(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return '';
  if (trimmed.length <= 8) return '[redacted]';
  return `${trimmed.slice(0, 4)}...[redacted]`;
}

export function redactedDestinationUrl(serverUrl: string, streamKey: string) {
  const normalizedServerUrl = normalizeRtmpServerUrl(serverUrl);
  return `${normalizedServerUrl}${redactStreamKey(streamKey)}`;
}
