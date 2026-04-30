import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { convertFileSrc, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { UnlistenFn } from '@tauri-apps/api/event';
import {
  YOUTUBE_RTMPS_SERVER,
  normalizeRtmpServerUrl,
  parseClipboardDestination,
  redactedDestinationUrl
} from './destinations';

type SessionPhase = 'idle' | 'preview' | 'recording' | 'connecting' | 'live' | 'stopping' | 'error';
type DestinationKind = 'youtubeOauth' | 'youtubeRtmps' | 'manualRtmp' | 'recordOnly';
type PanelTab = 'transcript' | 'chat' | 'telemetry';
type ControlSectionId = 'inputs' | 'destination' | 'audio' | 'video' | 'recording' | 'overlays';

interface AvDevices {
  video: string[];
  audio: string[];
  videoSources?: VideoSource[];
}

type VideoSourceKind = 'camera' | 'screen';
type PipPosition = 'bottomRight' | 'bottomLeft' | 'topRight' | 'topLeft';

interface VideoSource {
  id: string;
  label: string;
  kind: VideoSourceKind;
  avfoundationName: string;
  index: number;
}

interface VideoFeedSelection {
  primary: VideoSource;
  pip: VideoSource | null;
  layout: {
    pipEnabled: boolean;
    pipPosition: PipPosition;
    pipSizePercent: number;
  };
}

interface DestinationConfig {
  kind: DestinationKind;
  label: string;
  manualDestinationId?: string | null;
  rtmpUrl?: string | null;
  streamKey?: string | null;
  broadcastId?: string | null;
  streamId?: string | null;
  liveChatId?: string | null;
}

interface ManualDestination {
  id: string;
  label: string;
  provider: string;
  serverUrl: string;
  hasSavedKey: boolean;
  lastUsedAt?: number | null;
  defaultPrivacyNote?: string | null;
  confirmedLiveEnabled: boolean;
}

interface ManualDestinationSaveRequest {
  id?: string | null;
  label: string;
  provider: string;
  serverUrl: string;
  streamKey?: string | null;
  defaultPrivacyNote?: string | null;
  confirmedLiveEnabled: boolean;
}

interface ManualDestinationTestInput {
  serverUrl: string;
  streamKey?: string | null;
}

interface ManualDestinationTestRequest {
  destinationId?: string | null;
  inlineDestination?: ManualDestinationTestInput | null;
}

interface DestinationTestResult {
  ok: boolean;
  normalizedServerUrl?: string | null;
  redactedUrl?: string | null;
  message: string;
}

interface RecordingProfile {
  enabled: boolean;
  directory?: string | null;
  filenamePrefix: string;
  compactAfterStop: boolean;
}

interface EncoderProfile {
  width: number;
  height: number;
  fps: number;
  videoBitrateKbps: number;
  audioBitrateKbps: number;
  hevcCompact: boolean;
}

interface OverlayItem {
  id: string;
  name: string;
  sourcePath?: string | null;
  assetPath: string;
  x: number;
  y: number;
  scale: number;
  opacity: number;
  zIndex: number;
  visible: boolean;
}

interface AudioFilters {
  noiseSuppression: boolean;
  noiseSuppressionLevel: number;
  compressor: boolean;
  noiseGate: boolean;
  noiseGateThresholdDb: number;
  gainDb: number;
}

interface VideoCorrection {
  enabled: boolean;
  brightness: number;
  contrast: number;
  saturation: number;
  gamma: number;
}

interface FfmpegTelemetry {
  frame?: number | null;
  fps?: number | null;
  bitrateKbps?: number | null;
  speed?: number | null;
  droppedFrames: number;
  errors: number;
  lastLine?: string | null;
  exitReason?: string | null;
}

interface StreamStatus {
  phase: SessionPhase;
  destination: string;
  message: string;
}

interface RecordingStatus {
  path?: string | null;
  compactedPath?: string | null;
  durationMs: number;
  bytesWritten: number;
  bitrateKbps?: number | null;
  compactedBytes?: number | null;
}

interface YoutubeStatus {
  connected: boolean;
  message: string;
  broadcastId?: string | null;
  streamId?: string | null;
  liveChatId?: string | null;
  ingestUrl?: string | null;
  streamKey?: string | null;
}

interface ChatMessage {
  id: string;
  author: string;
  message: string;
  role?: string | null;
  publishedAt?: string | null;
  amountDisplay?: string | null;
  isSuperChat: boolean;
}

interface TranscriptWord {
  text: string;
  normalizedText: string;
  confidence: number;
  startMs: number;
  endMs: number;
  sourceModel: string;
  chunkId: number;
}

interface TranscriptAlternate {
  modelName: string;
  confidence: number;
  text: string;
  words: TranscriptWord[];
}

interface TranscriptSegment {
  id: string;
  chunkId: number;
  startMs: number;
  endMs: number;
  confidence: number;
  sourceModel: string;
  text: string;
  words: TranscriptWord[];
  alternates: TranscriptAlternate[];
}

interface TranscriptFinalization {
  isFinal: boolean;
  finalizedAtMs?: number | null;
  sourceMediaPath?: string | null;
  finalMediaPath?: string | null;
  sidecarPaths: string[];
  audioSource?: string | null;
}

interface TranscriptDocument {
  schemaVersion: number;
  sessionId: string;
  sourceLabel: string;
  micId?: string | null;
  startedAtMs: number;
  updatedAtMs: number;
  finalization: TranscriptFinalization;
  segments: TranscriptSegment[];
}

interface TranscriptLiveUpdate {
  chunkId: number;
  text: string;
  confidence: number;
  sourceModel: string;
  startMs: number;
  endMs: number;
  updatedAtMs: number;
}

interface VideoEngineStatus {
  engine: string;
  depthOfField: boolean;
  fallbackActive: boolean;
  queueDepth: number;
  droppedFrames: number;
  message: string;
}

interface SessionStatus {
  phase: SessionPhase;
  destination?: DestinationConfig | null;
  recordingPath?: string | null;
  compactedPath?: string | null;
  startedAtMs?: number | null;
  durationMs: number;
  bytesWritten: number;
  compactedBytes?: number | null;
  bitrateKbps?: number | null;
  telemetry: FfmpegTelemetry;
  videoEngine: VideoEngineStatus;
  overlays: OverlayItem[];
  error?: string | null;
}

const DEFAULT_ENCODER: EncoderProfile = {
  width: 1920,
  height: 1080,
  fps: 30,
  videoBitrateKbps: 6000,
  audioBitrateKbps: 160,
  hevcCompact: true
};

const DEFAULT_RECORDING: RecordingProfile = {
  enabled: true,
  directory: null,
  filenamePrefix: 'stagebadger',
  compactAfterStop: true
};

const INITIAL_TELEMETRY: FfmpegTelemetry = {
  frame: null,
  fps: null,
  bitrateKbps: null,
  speed: null,
  droppedFrames: 0,
  errors: 0,
  lastLine: null,
  exitReason: null
};

const BUILT_IN_OVERLAYS: OverlayItem[] = [
  {
    id: 'cyberpunk',
    name: 'Cyberpunk',
    sourcePath: null,
    assetPath: '/overlays/cyberpunk.png',
    x: 0.5,
    y: 0.5,
    scale: 1,
    opacity: 1,
    zIndex: 1,
    visible: false
  },
  {
    id: 'corporate',
    name: 'Corporate',
    sourcePath: null,
    assetPath: '/overlays/corporate.png',
    x: 0.5,
    y: 0.5,
    scale: 1,
    opacity: 1,
    zIndex: 1,
    visible: false
  },
  {
    id: 'gaming',
    name: 'Gaming',
    sourcePath: null,
    assetPath: '/overlays/gaming.png',
    x: 0.5,
    y: 0.5,
    scale: 1,
    opacity: 1,
    zIndex: 1,
    visible: false
  }
];

const BRAND_LOGO_URL = new URL('../assets/stagebadgerlogo.png', import.meta.url).href;

function hasTauriBridge() {
  return Boolean((window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);
}

function classifyVideoSource(label: string): VideoSourceKind {
  return /capture screen|screen|display|desktop/i.test(label) ? 'screen' : 'camera';
}

function fallbackVideoSource(label: string, index: number): VideoSource {
  const kind = classifyVideoSource(label);
  return {
    id: `${kind}-${index}`,
    label,
    kind,
    avfoundationName: label,
    index
  };
}

function normalizeVideoSources(devices: AvDevices): VideoSource[] {
  if (devices.videoSources?.length) return devices.videoSources;
  return devices.video.map((label, index) => fallbackVideoSource(label, index));
}

function sameVideoFeed(a: VideoFeedSelection | null, b: VideoFeedSelection | null) {
  if (!a || !b) return a === b;
  return JSON.stringify(a) === JSON.stringify(b);
}

const PREVIEW_DESTINATION_STORAGE_KEY = 'stagebadger.manualDestinations';
const PREVIEW_DESTINATION_SECRET_PREFIX = 'stagebadger.manualDestinationKey.';
const PREVIEW_TRANSCRIPT_STORAGE_KEY = 'stagebadger.transcriptFixture';

function readPreviewDestinations(): ManualDestination[] {
  try {
    const raw = window.localStorage.getItem(PREVIEW_DESTINATION_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as ManualDestination[];
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function writePreviewDestinations(destinations: ManualDestination[]) {
  window.localStorage.setItem(PREVIEW_DESTINATION_STORAGE_KEY, JSON.stringify(destinations));
}

function readPreviewSecret(id: string) {
  return window.localStorage.getItem(`${PREVIEW_DESTINATION_SECRET_PREFIX}${id}`);
}

function writePreviewSecret(id: string, streamKey: string) {
  window.localStorage.setItem(`${PREVIEW_DESTINATION_SECRET_PREFIX}${id}`, streamKey);
}

function deletePreviewSecret(id: string) {
  window.localStorage.removeItem(`${PREVIEW_DESTINATION_SECRET_PREFIX}${id}`);
}

function previewDestinationId(label: string) {
  return `destination-${label.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '') || Date.now()}`;
}

function mockStatus(phase: SessionPhase, destination: DestinationConfig | null, recordingPath: string | null): SessionStatus {
  return {
    phase,
    destination,
    recordingPath,
    compactedPath: null,
    startedAtMs: Date.now(),
    durationMs: 0,
    bytesWritten: 0,
    compactedBytes: null,
    bitrateKbps: null,
    telemetry: INITIAL_TELEMETRY,
    videoEngine: {
      engine: 'direct-ffmpeg',
      depthOfField: false,
      fallbackActive: true,
      queueDepth: 0,
      droppedFrames: 0,
      message: 'Browser preview fallback active'
    },
    overlays: [],
    error: null
  };
}

async function safeInvoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  (window as Window & { __STAGEBADGER_LAST_COMMAND__?: { command: string; args?: Record<string, unknown> } }).__STAGEBADGER_LAST_COMMAND__ = { command, args };
  if (hasTauriBridge()) {
    return invoke<T>(command, args);
  }

  if (command === 'get_av_devices') {
    const video = ['Default Camera', 'Capture screen 0'];
    return {
      video,
      audio: ['Default Microphone'],
      videoSources: video.map((label, index) => fallbackVideoSource(label, index))
    } as T;
  }
  if (command === 'check_moe_mount') {
    return false as T;
  }
  if (command === 'connect_youtube') {
    return {
      connected: false,
      message: 'Browser preview: YouTube OAuth is not connected.',
      broadcastId: null,
      streamId: null,
      liveChatId: null,
      ingestUrl: null,
      streamKey: null
    } as T;
  }
  if (command === 'create_broadcast') {
    return {
      connected: false,
      message: 'Browser preview: use manual RTMP/key to simulate a live destination.',
      broadcastId: null,
      streamId: null,
      liveChatId: null,
      ingestUrl: null,
      streamKey: null
    } as T;
  }
  if (command === 'open_youtube_live_control_room') {
    window.open('https://studio.youtube.com/', '_blank', 'noopener,noreferrer');
    return undefined as T;
  }
  if (command === 'load_manual_destinations') {
    return readPreviewDestinations().map((destination) => ({
      ...destination,
      hasSavedKey: Boolean(readPreviewSecret(destination.id))
    })) as T;
  }
  if (command === 'save_manual_destination') {
    const request = args?.request as ManualDestinationSaveRequest | undefined;
    if (!request) throw new Error('Missing manual destination request.');
    const id = request.id?.trim() || previewDestinationId(request.label);
    const existing = readPreviewDestinations().find((item) => item.id === id);
    const existingSecret = readPreviewSecret(id);
    const destination: ManualDestination = {
      id,
      label: request.label.trim() || (request.provider === 'youtube' ? 'YouTube RTMPS' : 'Custom RTMP'),
      provider: request.provider,
      serverUrl: normalizeRtmpServerUrl(request.serverUrl),
      hasSavedKey: Boolean(request.streamKey?.trim() || existingSecret),
      lastUsedAt: Date.now(),
      defaultPrivacyNote: request.defaultPrivacyNote?.trim() || null,
      confirmedLiveEnabled: request.confirmedLiveEnabled
    };
    const destinations = readPreviewDestinations().filter((item) => item.id !== id).concat(destination);
    writePreviewDestinations(destinations);
    if (request.streamKey?.trim()) {
      writePreviewSecret(id, request.streamKey.trim());
    } else if (!existing && !existingSecret) {
      deletePreviewSecret(id);
    }
    return destination as T;
  }
  if (command === 'delete_manual_destination') {
    const destinationId = String(args?.destination_id ?? '');
    writePreviewDestinations(readPreviewDestinations().filter((destination) => destination.id !== destinationId));
    deletePreviewSecret(destinationId);
    return readPreviewDestinations() as T;
  }
  if (command === 'test_rtmp_destination') {
    const request = args?.request as ManualDestinationTestRequest | undefined;
    if (!request) throw new Error('Missing destination test request.');
    if (request.destinationId) {
      const destination = readPreviewDestinations().find((item) => item.id === request.destinationId);
      if (!destination) {
        return {
          ok: false,
          message: 'Saved destination was not found.',
          normalizedServerUrl: null,
          redactedUrl: null
        } as T;
      }
      const streamKey = readPreviewSecret(destination.id);
      return {
        ok: Boolean(streamKey),
        message: streamKey ? 'Destination details are valid locally.' : 'Saved destination is missing its keychain stream key.',
        normalizedServerUrl: normalizeRtmpServerUrl(destination.serverUrl),
        redactedUrl: streamKey ? redactedDestinationUrl(destination.serverUrl, streamKey) : null
      } as T;
    }
    if (request.inlineDestination) {
      const normalizedServerUrl = normalizeRtmpServerUrl(request.inlineDestination.serverUrl);
      const streamKey = request.inlineDestination.streamKey?.trim();
      return {
        ok: Boolean(streamKey),
        message: streamKey ? 'Destination details are valid locally.' : 'Enter a stream key before testing this destination.',
        normalizedServerUrl,
        redactedUrl: streamKey ? redactedDestinationUrl(normalizedServerUrl, streamKey) : null
      } as T;
    }
    return {
      ok: false,
      message: 'Provide either a saved destination id or inline destination details.',
      normalizedServerUrl: null,
      redactedUrl: null
    } as T;
  }
  if (command === 'start_live_session') {
    const request = args?.request as { destination?: DestinationConfig } | undefined;
    return mockStatus('live', request?.destination ?? null, '/Users/you/Movies/StageBadger/stagebadger-preview.mp4') as T;
  }
  if (command === 'start_recording') {
    return mockStatus('recording', { kind: 'recordOnly', label: 'Local recording' }, '/Users/you/Movies/StageBadger/stagebadger-recording.mp4') as T;
  }
  if (command === 'stop_session') {
    return mockStatus('idle', null, '/Users/you/Movies/StageBadger/stagebadger-recording.mp4') as T;
  }
  if (command === 'get_session_status') {
    return mockStatus('idle', null, null) as T;
  }
  if (command === 'set_overlay_state') {
    return { overlays: [args?.overlay], message: 'Overlay state updated' } as T;
  }

  throw new Error(`Command ${command} is unavailable in browser preview.`);
}

function phaseLabel(phase: SessionPhase) {
  return phase.replace(/([a-z])([A-Z])/g, '$1 $2').toUpperCase();
}

function formatDuration(ms: number) {
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return [hours, minutes, seconds].map((value) => value.toString().padStart(2, '0')).join(':');
}

function formatBytes(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(value >= 10 ? 1 : 2)} ${units[unitIndex]}`;
}

function overlaySrc(path: string) {
  if (path.startsWith('/overlays/') || path.startsWith('blob:') || path.startsWith('data:')) {
    return path;
  }
  return hasTauriBridge() ? convertFileSrc(path) : path;
}

function confidenceClass(prob: number) {
  if (prob > 0.9) return 'high';
  if (prob > 0.6) return 'medium';
  return 'low';
}

function formatTranscriptTimestamp(ms: number) {
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const millis = Math.floor(ms % 1000);
  return `${hours.toString().padStart(2, '0')}:${minutes.toString().padStart(2, '0')}:${seconds.toString().padStart(2, '0')}.${millis.toString().padStart(3, '0')}`;
}

function readPreviewTranscriptFixture(): TranscriptDocument | null {
  try {
    const raw = window.localStorage.getItem(PREVIEW_TRANSCRIPT_STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as TranscriptDocument;
    if (!parsed || !Array.isArray(parsed.segments)) return null;
    return parsed;
  } catch {
    return null;
  }
}

interface ControlAccordionSectionProps {
  id: ControlSectionId;
  title: string;
  summary: string;
  summaryOk?: boolean;
  openSection: ControlSectionId | null;
  onToggle: (id: ControlSectionId) => void;
  children: ReactNode;
}

function ControlAccordionSection({
  id,
  title,
  summary,
  summaryOk = false,
  openSection,
  onToggle,
  children
}: ControlAccordionSectionProps) {
  const expanded = openSection === id;
  const panelId = `control-panel-${id}`;
  const headingId = `control-heading-${id}`;

  return (
    <section className={expanded ? 'control-accordion open' : 'control-accordion'} aria-labelledby={headingId}>
      <h2 id={headingId} className="accordion-heading">
        <button
          type="button"
          className="accordion-trigger"
          aria-label={title}
          aria-expanded={expanded}
          aria-controls={panelId}
          onClick={() => onToggle(id)}
        >
          <span className="accordion-title">{title}</span>
          <span className={summaryOk ? 'pill accordion-summary ok' : 'pill accordion-summary'}>{summary}</span>
          <span className="accordion-caret" aria-hidden="true" />
        </button>
      </h2>
      {expanded ? (
        <div id={panelId} className="accordion-panel">
          {children}
        </div>
      ) : null}
    </section>
  );
}

export default function App() {
  const [devices, setDevices] = useState<AvDevices>({ video: [], audio: [] });
  const [selectedPrimarySourceId, setSelectedPrimarySourceId] = useState('');
  const [pipEnabled, setPipEnabled] = useState(false);
  const [selectedPipSourceId, setSelectedPipSourceId] = useState('');
  const [pipPosition, setPipPosition] = useState<PipPosition>('bottomRight');
  const [pipSizePercent, setPipSizePercent] = useState(24);
  const [activeOutputFeeds, setActiveOutputFeeds] = useState<VideoFeedSelection | null>(null);
  const [selectedMic, setSelectedMic] = useState('0');
  const [phase, setPhase] = useState<SessionPhase>('idle');
  const [destinationMode, setDestinationMode] = useState<'youtube' | 'manual'>('manual');
  const [manualServerUrl, setManualServerUrl] = useState(YOUTUBE_RTMPS_SERVER);
  const [manualStreamKey, setManualStreamKey] = useState('');
  const [manualDestinationId, setManualDestinationId] = useState<string | null>(null);
  const [manualDestinationLabel, setManualDestinationLabel] = useState('YouTube RTMPS');
  const [manualPrivacyNote, setManualPrivacyNote] = useState('Confirm privacy in YouTube Studio');
  const [manualConfirmedLiveEnabled, setManualConfirmedLiveEnabled] = useState(false);
  const [manualDestinations, setManualDestinations] = useState<ManualDestination[]>([]);
  const [destinationTest, setDestinationTest] = useState<DestinationTestResult | null>(null);
  const [broadcastTitle, setBroadcastTitle] = useState('StageBadger Live');
  const [privacyStatus, setPrivacyStatus] = useState('unlisted');
  const [youtubeStatus, setYoutubeStatus] = useState<YoutubeStatus | null>(null);
  const [recording, setRecording] = useState<RecordingProfile>(DEFAULT_RECORDING);
  const [encoder, setEncoder] = useState<EncoderProfile>(DEFAULT_ENCODER);
  const [overlays, setOverlays] = useState<OverlayItem[]>(BUILT_IN_OVERLAYS);
  const [activeOverlayId, setActiveOverlayId] = useState<string | null>(null);
  const [depthOfField, setDepthOfField] = useState(false);
  const [activeTab, setActiveTab] = useState<PanelTab>('transcript');
  const [sessionStatus, setSessionStatus] = useState<SessionStatus>(mockStatus('idle', null, null));
  const [recordingStatus, setRecordingStatus] = useState<RecordingStatus>({
    path: null,
    compactedPath: null,
    durationMs: 0,
    bytesWritten: 0,
    bitrateKbps: null,
    compactedBytes: null
  });
  const [telemetry, setTelemetry] = useState<FfmpegTelemetry>(INITIAL_TELEMETRY);
  const [chatMessages, setChatMessages] = useState<ChatMessage[]>([]);
  const [transcript, setTranscript] = useState<TranscriptDocument>({
    schemaVersion: 1,
    sessionId: 'preview',
    sourceLabel: 'StageBadger',
    micId: null,
    startedAtMs: Date.now(),
    updatedAtMs: Date.now(),
    finalization: {
      isFinal: false,
      finalizedAtMs: null,
      sourceMediaPath: null,
      finalMediaPath: null,
      sidecarPaths: [],
      audioSource: null
    },
    segments: []
  });
  const [liveTranscript, setLiveTranscript] = useState<TranscriptLiveUpdate | null>(null);
  const [logs, setLogs] = useState<{ id: number; message: string }[]>([]);
  const [inlineError, setInlineError] = useState<string | null>(null);
  const [audioFilters, setAudioFilters] = useState<AudioFilters>({
    noiseSuppression: false,
    noiseSuppressionLevel: 0.5,
    compressor: false,
    noiseGate: false,
    noiseGateThresholdDb: -30,
    gainDb: 0,
  });
  const [videoCorrection, setVideoCorrection] = useState<VideoCorrection>({
    enabled: false,
    brightness: 0,
    contrast: 1,
    saturation: 1,
    gamma: 1,
  });
  const [openControlSection, setOpenControlSection] = useState<ControlSectionId | null>(null);
  const [micLevel, setMicLevel] = useState(0);
  const primaryVideoRef = useRef<HTMLVideoElement>(null);
  const pipVideoRef = useRef<HTMLVideoElement>(null);
  const screenPreviewCacheRef = useRef<Map<string, MediaStream>>(new Map());
  const logIdRef = useRef(0);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const audioContextRef = useRef<AudioContext | null>(null);

  const activeOverlay = overlays.find((overlay) => overlay.id === activeOverlayId) ?? null;
  const visibleOverlays = useMemo(() => overlays.filter((overlay) => overlay.visible), [overlays]);
  const videoSources = useMemo(() => normalizeVideoSources(devices), [devices]);
  const cameraSources = useMemo(() => videoSources.filter((source) => source.kind === 'camera'), [videoSources]);
  const screenSources = useMemo(() => videoSources.filter((source) => source.kind === 'screen'), [videoSources]);
  const selectedPrimarySource = useMemo(
    () => videoSources.find((source) => source.id === selectedPrimarySourceId) ?? cameraSources[0] ?? videoSources[0] ?? null,
    [cameraSources, selectedPrimarySourceId, videoSources]
  );
  const pipCandidates = useMemo(
    () => videoSources.filter((source) => source.id !== selectedPrimarySource?.id),
    [selectedPrimarySource?.id, videoSources]
  );
  const selectedPipSource = useMemo(
    () => pipCandidates.find((source) => source.id === selectedPipSourceId) ?? pipCandidates[0] ?? null,
    [pipCandidates, selectedPipSourceId]
  );
  const videoFeeds = useMemo<VideoFeedSelection | null>(() => {
    if (!selectedPrimarySource) return null;
    return {
      primary: selectedPrimarySource,
      pip: pipEnabled ? selectedPipSource : null,
      layout: {
        pipEnabled: pipEnabled && Boolean(selectedPipSource),
        pipPosition,
        pipSizePercent: Math.min(35, Math.max(18, pipSizePercent))
      }
    };
  }, [pipEnabled, pipPosition, pipSizePercent, selectedPipSource, selectedPrimarySource]);
  const selectedManualDestination = useMemo(
    () => manualDestinations.find((destination) => destination.id === manualDestinationId) ?? null,
    [manualDestinationId, manualDestinations]
  );
  const isBusy = phase === 'connecting' || phase === 'stopping';
  const isRunning = phase === 'live' || phase === 'recording' || phase === 'connecting';
  const outputLocked = isRunning && activeOutputFeeds !== null && !sameVideoFeed(activeOutputFeeds, videoFeeds);
  const defaultRecordingDirectory = recording.directory || '~/Movies/StageBadger';

  const pushLog = useCallback((message: string) => {
    setLogs((current) => [
      ...current.slice(-80),
      { id: logIdRef.current++, message: `[${new Date().toLocaleTimeString()}] ${message}` }
    ]);
  }, []);

  const applySessionStatus = useCallback((status: SessionStatus) => {
    setSessionStatus(status);
    setPhase(status.phase);
    setTelemetry(status.telemetry);
    setRecordingStatus({
      path: status.recordingPath,
      compactedPath: status.compactedPath,
      durationMs: status.durationMs,
      bytesWritten: status.bytesWritten,
      bitrateKbps: status.bitrateKbps,
      compactedBytes: status.compactedBytes
    });
    if (status.error) {
      setInlineError(status.error);
    }
    if (status.phase === 'idle' || status.phase === 'error') {
      setLiveTranscript(null);
      setActiveOutputFeeds(null);
    }
  }, []);

  const refreshDevices = useCallback(async () => {
    try {
      const nextDevices = await safeInvoke<AvDevices>('get_av_devices');
      const nextSources = normalizeVideoSources(nextDevices);
      setDevices(nextDevices);
      setSelectedPrimarySourceId((current) => {
        if (nextSources.some((source) => source.id === current)) return current;
        return nextSources.find((source) => source.kind === 'camera')?.id ?? nextSources[0]?.id ?? '';
      });
      if (nextDevices.audio[0] && selectedMic === '0') setSelectedMic(nextDevices.audio[0]);
      pushLog(`Detected ${nextDevices.video.length} video sources and ${nextDevices.audio.length} audio sources.`);
    } catch (error) {
      setInlineError(`Device refresh failed: ${String(error)}`);
    }
  }, [pushLog, selectedMic]);

  useEffect(() => {
    refreshDevices();
    safeInvoke<boolean>('check_moe_mount')
      .then((isMounted) => {
        setRecording((current) => ({
          ...current,
          directory: isMounted ? '/Volumes/MOE/StageBadger/Recordings' : null
        }));
      })
      .catch(() => undefined);
  }, [refreshDevices]);

  useEffect(() => {
    if (!selectedPrimarySource && videoSources[0]) {
      setSelectedPrimarySourceId(videoSources.find((source) => source.kind === 'camera')?.id ?? videoSources[0].id);
    }
  }, [selectedPrimarySource, videoSources]);

  useEffect(() => {
    if (!pipEnabled || !selectedPrimarySource) return;
    const preferredKind: VideoSourceKind = selectedPrimarySource.kind === 'screen' ? 'camera' : 'screen';
    const currentValid = pipCandidates.some((source) => source.id === selectedPipSourceId);
    if (currentValid) return;
    setSelectedPipSourceId(
      pipCandidates.find((source) => source.kind === preferredKind)?.id ?? pipCandidates[0]?.id ?? ''
    );
  }, [pipCandidates, pipEnabled, selectedPipSourceId, selectedPrimarySource]);

  const refreshManualDestinations = useCallback(async () => {
    try {
      const nextDestinations = await safeInvoke<ManualDestination[]>('load_manual_destinations');
      setManualDestinations(nextDestinations);
      if (!manualDestinationId && nextDestinations[0]) {
        setManualDestinationId(nextDestinations[0].id);
        setManualDestinationLabel(nextDestinations[0].label);
        setManualServerUrl(nextDestinations[0].serverUrl);
        setManualPrivacyNote(nextDestinations[0].defaultPrivacyNote ?? 'Confirm privacy in YouTube Studio');
        setManualConfirmedLiveEnabled(Boolean(nextDestinations[0].confirmedLiveEnabled));
      }
    } catch (error) {
      pushLog(`Manual destination load failed: ${String(error)}`);
    }
  }, [manualDestinationId, pushLog]);

  useEffect(() => {
    refreshManualDestinations();
  }, [refreshManualDestinations]);

  useEffect(() => {
    if (hasTauriBridge()) return;
    const fixture = readPreviewTranscriptFixture();
    if (fixture) {
      setTranscript(fixture);
    }
  }, []);

  const attachPreviewStream = useCallback(async (
    source: VideoSource,
    videoElement: HTMLVideoElement | null,
    role: 'Program' | 'In-screen'
  ) => {
    if (!videoElement || !navigator.mediaDevices) {
      pushLog('Camera/screen preview unavailable in this browser context.');
      return;
    }

    try {
      let stream: MediaStream;
      if (source.kind === 'screen') {
        const cached = screenPreviewCacheRef.current.get(source.id);
        if (cached && cached.getVideoTracks().some((track) => track.readyState === 'live')) {
          stream = cached;
        } else {
          pushLog(`Preview: Requesting screen capture for "${source.label}".`);
          stream = await navigator.mediaDevices.getDisplayMedia({
            video: { width: { ideal: 1920 }, height: { ideal: 1080 }, frameRate: { ideal: 30 } },
            audio: false
          });
          screenPreviewCacheRef.current.set(source.id, stream);
          stream.getVideoTracks().forEach((track) => {
            track.onended = () => {
              screenPreviewCacheRef.current.delete(source.id);
              setInlineError(`Screen share ended for ${source.label}. Select it again to resume preview.`);
            };
          });
        }
      } else {
        const devicesList = await navigator.mediaDevices.enumerateDevices().catch(() => []);
        const matchingDevice = devicesList.find((device) => (
          device.kind === 'videoinput' && device.label && source.label && device.label.includes(source.label)
        ));
        if (!matchingDevice) {
          pushLog(`Preview: Exact camera match unavailable for "${source.label}", using the default camera.`);
        }
        stream = await navigator.mediaDevices.getUserMedia({
          video: matchingDevice
            ? { deviceId: { exact: matchingDevice.deviceId }, width: { ideal: 1920 }, height: { ideal: 1080 } }
            : { width: { ideal: 1920 }, height: { ideal: 1080 } },
          audio: false
        });
      }

      const previous = videoElement.srcObject as MediaStream | null;
      videoElement.srcObject = stream;
      await videoElement.play().catch(() => undefined);
      if (previous && previous !== stream && !Array.from(screenPreviewCacheRef.current.values()).includes(previous)) {
        previous.getTracks().forEach((track) => track.stop());
      }
      const settings = stream.getVideoTracks()[0]?.getSettings();
      pushLog(`Preview: ${role} ${source.kind} active (${settings?.width ?? '?'}x${settings?.height ?? '?'}).`);
    } catch (error) {
      const message = source.kind === 'screen'
        ? `Screen capture unavailable for ${source.label}: ${String(error)}`
        : `Camera unavailable for ${source.label}: ${String(error)}`;
      setInlineError(message);
      pushLog(`Preview: ${message}`);
    }
  }, [pushLog]);

  useEffect(() => {
    if (!selectedPrimarySource) return;
    if (selectedPrimarySource.kind === 'screen') {
      const cached = screenPreviewCacheRef.current.get(selectedPrimarySource.id);
      if (cached && cached.getVideoTracks().some((track) => track.readyState === 'live')) {
        attachPreviewStream(selectedPrimarySource, primaryVideoRef.current, 'Program');
        return;
      }
      if (primaryVideoRef.current) {
        const previous = primaryVideoRef.current.srcObject as MediaStream | null;
        primaryVideoRef.current.srcObject = null;
        if (previous && !Array.from(screenPreviewCacheRef.current.values()).includes(previous)) {
          previous.getTracks().forEach((track) => track.stop());
        }
      }
      pushLog(`Preview: click the Preview button to authorize screen capture for "${selectedPrimarySource.label}".`);
      return;
    }
    attachPreviewStream(selectedPrimarySource, primaryVideoRef.current, 'Program');
  }, [attachPreviewStream, pushLog, selectedPrimarySource]);

  useEffect(() => {
    if (!videoFeeds?.layout.pipEnabled || !videoFeeds.pip) {
      if (pipVideoRef.current) pipVideoRef.current.srcObject = null;
      return;
    }
    if (videoFeeds.pip.kind === 'screen') {
      const cached = screenPreviewCacheRef.current.get(videoFeeds.pip.id);
      if (cached && cached.getVideoTracks().some((track) => track.readyState === 'live')) {
        attachPreviewStream(videoFeeds.pip, pipVideoRef.current, 'In-screen');
        return;
      }
      if (pipVideoRef.current) {
        const previous = pipVideoRef.current.srcObject as MediaStream | null;
        pipVideoRef.current.srcObject = null;
        if (previous && !Array.from(screenPreviewCacheRef.current.values()).includes(previous)) {
          previous.getTracks().forEach((track) => track.stop());
        }
      }
      pushLog(`Preview: click the Preview button to authorize screen capture for "${videoFeeds.pip.label}".`);
      return;
    }
    attachPreviewStream(videoFeeds.pip, pipVideoRef.current, 'In-screen');
  }, [attachPreviewStream, pushLog, videoFeeds]);

  // VU meter — capture mic audio and compute RMS level for visual feedback
  useEffect(() => {
    if (!navigator.mediaDevices) return undefined;
    let stopped = false;
    let rafId: number;

    navigator.mediaDevices.getUserMedia({ audio: true, video: false }).then((stream) => {
      if (stopped) { stream.getTracks().forEach((t) => t.stop()); return; }
      const ctx = new AudioContext();
      const source = ctx.createMediaStreamSource(stream);
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 256;
      analyser.smoothingTimeConstant = 0.8;
      source.connect(analyser);
      audioContextRef.current = ctx;
      analyserRef.current = analyser;

      const data = new Uint8Array(analyser.frequencyBinCount);
      let frame = 0;
      const poll = () => {
        if (stopped) return;
        frame++;
        // Update at ~15fps to avoid excessive renders
        if (frame % 2 === 0) {
          analyser.getByteFrequencyData(data);
          let sum = 0;
          for (let i = 0; i < data.length; i++) sum += data[i] * data[i];
          const rms = Math.sqrt(sum / data.length) / 255;
          setMicLevel(rms);
        }
        rafId = requestAnimationFrame(poll);
      };
      poll();
    }).catch(() => undefined);

    return () => {
      stopped = true;
      cancelAnimationFrame(rafId);
      audioContextRef.current?.close();
      audioContextRef.current = null;
      analyserRef.current = null;
    };
  }, []);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    const attach = async () => {
      try {
        unlisteners.push(await listen<StreamStatus>('stream_status', (event) => {
          setPhase(event.payload.phase);
          pushLog(`${event.payload.destination}: ${event.payload.message}`);
        }));
        unlisteners.push(await listen<RecordingStatus>('recording_status', (event) => {
          setRecordingStatus(event.payload);
        }));
        unlisteners.push(await listen<YoutubeStatus>('youtube_status', (event) => {
          setYoutubeStatus(event.payload);
          pushLog(`YouTube: ${event.payload.message}`);
        }));
        unlisteners.push(await listen<ChatMessage>('chat_message', (event) => {
          setChatMessages((current) => [...current.slice(-99), event.payload]);
        }));
        unlisteners.push(await listen<TranscriptDocument>('transcript_update', (event) => {
          setTranscript(event.payload);
        }));
        unlisteners.push(await listen<TranscriptLiveUpdate>('transcript_live', (event) => {
          setLiveTranscript(event.payload.text.trim() ? event.payload : null);
        }));
        unlisteners.push(await listen<FfmpegTelemetry>('ffmpeg_telemetry', (event) => {
          setTelemetry(event.payload);
        }));
        unlisteners.push(await listen<string>('system_log', (event) => {
          pushLog(event.payload);
        }));
      } catch {
        pushLog('Browser preview bridge active; Tauri event listeners are disabled.');
      }
    };
    attach();
    return () => unlisteners.forEach((unlisten) => unlisten());
  }, [pushLog]);

  useEffect(() => {
    if (!isRunning) return undefined;
    const timer = window.setInterval(() => {
      safeInvoke<SessionStatus>('get_session_status')
        .then(applySessionStatus)
        .catch(() => undefined);
    }, 1500);
    return () => window.clearInterval(timer);
  }, [applySessionStatus, isRunning]);

  const updateOverlay = useCallback((nextOverlay: OverlayItem) => {
    setOverlays((current) => current.map((overlay) => (overlay.id === nextOverlay.id ? nextOverlay : overlay)));
    safeInvoke('set_overlay_state', { overlay: nextOverlay }).catch((error) => {
      pushLog(`Overlay update stayed local: ${String(error)}`);
    });
  }, [pushLog]);

  const selectOverlay = useCallback((overlayId: string | null) => {
    setActiveOverlayId(overlayId);
    setOverlays((current) => current.map((overlay) => ({
      ...overlay,
      visible: overlay.id === overlayId
    })));
  }, []);

  const importOverlay = useCallback((file: File) => {
    const sourcePath = (file as File & { path?: string }).path;
    const previewUrl = URL.createObjectURL(file);
    const localOverlay: OverlayItem = {
      id: `local-${Date.now()}`,
      name: file.name,
      sourcePath: sourcePath ?? null,
      assetPath: previewUrl,
      x: 0.5,
      y: 0.5,
      scale: 1,
      opacity: 1,
      zIndex: overlays.length + 1,
      visible: true
    };
    setOverlays((current) => current.map((overlay) => ({ ...overlay, visible: false })).concat(localOverlay));
    setActiveOverlayId(localOverlay.id);
    if (sourcePath) {
      safeInvoke<{ overlays: OverlayItem[]; message: string }>('add_overlay_asset', { sourcePath, name: file.name })
        .then((status) => {
          setOverlays(status.overlays);
          setActiveOverlayId(status.overlays[status.overlays.length - 1]?.id ?? localOverlay.id);
          pushLog(status.message);
        })
        .catch((error) => setInlineError(`Overlay import failed: ${String(error)}`));
    }
  }, [overlays.length, pushLog]);

  const connectYoutube = useCallback(async () => {
    setInlineError(null);
    const status = await safeInvoke<YoutubeStatus>('connect_youtube');
    setYoutubeStatus(status);
    pushLog(status.message);
    return status;
  }, [pushLog]);

  const createYoutubeBroadcast = useCallback(async () => {
    const status = await safeInvoke<YoutubeStatus>('create_broadcast', {
      title: broadcastTitle,
      privacyStatus
    });
    setYoutubeStatus(status);
    pushLog(status.message);
    return status;
  }, [broadcastTitle, privacyStatus, pushLog]);

  const openYoutubeLiveControlRoom = useCallback(async () => {
    await safeInvoke('open_youtube_live_control_room');
    pushLog('Opened YouTube Live Control Room.');
  }, [pushLog]);

  const pasteStreamDetails = useCallback(async () => {
    try {
      const clipboard = await navigator.clipboard.readText();
      const parsed = parseClipboardDestination(clipboard, manualServerUrl);
      setManualServerUrl(parsed.serverUrl);
      setManualStreamKey(parsed.streamKey);
      setDestinationTest(null);
      pushLog('Clipboard stream details imported.');
    } catch (error) {
      setInlineError(`Clipboard import failed: ${String(error)}`);
    }
  }, [manualServerUrl, pushLog]);

  const saveManualDestination = useCallback(async () => {
    try {
      setInlineError(null);
      const normalizedServerUrl = normalizeRtmpServerUrl(manualServerUrl);
      const request: ManualDestinationSaveRequest = {
        id: manualDestinationId,
        label: manualDestinationLabel.trim() || 'YouTube RTMPS',
        provider: normalizedServerUrl === YOUTUBE_RTMPS_SERVER ? 'youtube' : 'custom',
        serverUrl: normalizedServerUrl,
        streamKey: manualStreamKey,
        defaultPrivacyNote: manualPrivacyNote,
        confirmedLiveEnabled: manualConfirmedLiveEnabled
      };
      const saved = await safeInvoke<ManualDestination>('save_manual_destination', { request });
      setManualDestinations((current) => {
        const next = current.filter((item) => item.id !== saved.id).concat(saved);
        next.sort((a, b) => (b.lastUsedAt ?? 0) - (a.lastUsedAt ?? 0) || a.label.localeCompare(b.label));
        return next;
      });
      setManualDestinationId(saved.id);
      setManualDestinationLabel(saved.label);
      setManualServerUrl(saved.serverUrl);
      setManualStreamKey('');
      setDestinationTest(null);
      pushLog(`Saved ${saved.label}. Stream key stored in keychain.`);
    } catch (error) {
      setInlineError(`Save failed: ${String(error)}`);
    }
  }, [
    manualConfirmedLiveEnabled,
    manualDestinationId,
    manualDestinationLabel,
    manualPrivacyNote,
    manualServerUrl,
    manualStreamKey,
    pushLog
  ]);

  const deleteManualDestination = useCallback(async (destinationId: string) => {
    try {
      const next = await safeInvoke<ManualDestination[]>('delete_manual_destination', { destination_id: destinationId });
      setManualDestinations(next);
      if (manualDestinationId === destinationId) {
        const nextSelection = next[0] ?? null;
        setManualDestinationId(nextSelection?.id ?? null);
        setManualDestinationLabel(nextSelection?.label ?? 'YouTube RTMPS');
        setManualServerUrl(nextSelection?.serverUrl ?? YOUTUBE_RTMPS_SERVER);
        setManualStreamKey('');
        setManualPrivacyNote(nextSelection?.defaultPrivacyNote ?? 'Confirm privacy in YouTube Studio');
        setManualConfirmedLiveEnabled(Boolean(nextSelection?.confirmedLiveEnabled));
      }
      pushLog('Saved destination deleted.');
    } catch (error) {
      setInlineError(`Delete failed: ${String(error)}`);
    }
  }, [manualDestinationId, pushLog]);

  const testManualDestination = useCallback(async () => {
    try {
      const request: ManualDestinationTestRequest = manualDestinationId
        ? { destinationId: manualDestinationId, inlineDestination: null }
        : { destinationId: null, inlineDestination: { serverUrl: manualServerUrl, streamKey: manualStreamKey } };
      const result = await safeInvoke<DestinationTestResult>('test_rtmp_destination', { request });
      setDestinationTest(result);
      pushLog(result.message);
    } catch (error) {
      setInlineError(`Destination test failed: ${String(error)}`);
    }
  }, [manualDestinationId, manualServerUrl, manualStreamKey, pushLog]);

  const selectManualDestination = useCallback((destination: ManualDestination | null) => {
    setDestinationMode('manual');
    setManualDestinationId(destination?.id ?? null);
    setManualDestinationLabel(destination?.label ?? 'YouTube RTMPS');
    setManualServerUrl(destination?.serverUrl ?? YOUTUBE_RTMPS_SERVER);
    setManualStreamKey('');
    setManualPrivacyNote(destination?.defaultPrivacyNote ?? 'Confirm privacy in YouTube Studio');
    setManualConfirmedLiveEnabled(Boolean(destination?.confirmedLiveEnabled));
    setDestinationTest(null);
  }, []);

  const buildDestination = useCallback(async (): Promise<DestinationConfig> => {
    if (destinationMode === 'manual') {
      if (selectedManualDestination) {
        if (!selectedManualDestination.hasSavedKey) {
          throw new Error(`Saved destination "${selectedManualDestination.label}" is missing its stream key.`);
        }
        return {
          kind: 'youtubeRtmps',
          label: selectedManualDestination.label,
          manualDestinationId: selectedManualDestination.id,
          rtmpUrl: normalizeRtmpServerUrl(selectedManualDestination.serverUrl),
          streamKey: null,
          broadcastId: null,
          streamId: null,
          liveChatId: null
        };
      }

      if (!manualStreamKey.trim()) {
        throw new Error('Enter a stream key or save a YouTube RTMPS destination first.');
      }
      const normalizedServerUrl = normalizeRtmpServerUrl(manualServerUrl);
      return {
        kind: 'manualRtmp',
        label: manualDestinationLabel.trim() || 'Manual RTMP',
        manualDestinationId: null,
        rtmpUrl: normalizedServerUrl,
        streamKey: manualStreamKey,
        broadcastId: null,
        streamId: null,
        liveChatId: null
      };
    }

    const connected = youtubeStatus?.connected ? youtubeStatus : await connectYoutube();
    const broadcast = connected.ingestUrl && connected.streamKey ? connected : await createYoutubeBroadcast();
    if (!broadcast.ingestUrl || !broadcast.streamKey) {
      throw new Error(broadcast.message);
    }
    return {
      kind: 'youtubeOauth',
      label: 'YouTube OAuth RTMPS',
      rtmpUrl: broadcast.ingestUrl.endsWith('/') ? broadcast.ingestUrl : `${broadcast.ingestUrl}/`,
      streamKey: broadcast.streamKey,
      broadcastId: broadcast.broadcastId,
      streamId: broadcast.streamId,
      liveChatId: broadcast.liveChatId
    };
  }, [
    connectYoutube,
    createYoutubeBroadcast,
    destinationMode,
    manualDestinationLabel,
    manualServerUrl,
    manualStreamKey,
    selectedManualDestination,
    youtubeStatus
  ]);

  const startLive = useCallback(async () => {
    try {
      if (!videoFeeds) throw new Error('Select a program feed before starting.');
      setInlineError(null);
      setLiveTranscript(null);
      setPhase('connecting');
      const destination = await buildDestination();
      setActiveOutputFeeds(videoFeeds);
      const status = await safeInvoke<SessionStatus>('start_live_session', {
        request: {
          destination,
          recording: { ...recording, enabled: true },
          encoder,
          cameraId: videoFeeds.primary.avfoundationName,
          videoFeeds,
          micId: selectedMic,
          overlays: visibleOverlays,
          depthOfField,
          audioFilters,
          videoCorrection
        }
      });
      applySessionStatus(status);
      pushLog(`Live session started for ${destination.label}.`);
    } catch (error) {
      setPhase('error');
      setActiveOutputFeeds(null);
      setInlineError(String(error));
      pushLog(`Live start failed: ${String(error)}`);
    }
  }, [applySessionStatus, audioFilters, buildDestination, depthOfField, encoder, pushLog, recording, selectedMic, videoCorrection, videoFeeds, visibleOverlays]);

  const startRecordOnly = useCallback(async () => {
    try {
      if (!videoFeeds) throw new Error('Select a program feed before recording.');
      setInlineError(null);
      setLiveTranscript(null);
      setPhase('connecting');
      setActiveOutputFeeds(videoFeeds);
      const status = await safeInvoke<SessionStatus>('start_recording', {
        cameraId: videoFeeds.primary.avfoundationName,
        videoFeeds,
        micId: selectedMic,
        recording: { ...recording, enabled: true },
        encoder,
        overlays: visibleOverlays,
        depthOfField
      });
      applySessionStatus(status);
      pushLog('Record-only session started.');
    } catch (error) {
      setPhase('error');
      setActiveOutputFeeds(null);
      setInlineError(String(error));
    }
  }, [applySessionStatus, depthOfField, encoder, pushLog, recording, selectedMic, videoFeeds, visibleOverlays]);

  const stopSession = useCallback(async () => {
    try {
      setPhase('stopping');
      const status = await safeInvoke<SessionStatus>('stop_session', { compactAfterStop: recording.compactAfterStop });
      applySessionStatus(status);
      setActiveOutputFeeds(null);
      pushLog('Session stopped.');
    } catch (error) {
      setPhase('error');
      setInlineError(`Stop failed: ${String(error)}`);
    }
  }, [applySessionStatus, pushLog, recording.compactAfterStop]);

  const setPreview = useCallback(async () => {
    setInlineError(null);
    setPhase('preview');
    pushLog('Preview armed. No network output is active.');
    const previewTasks: Promise<void>[] = [];
    if (selectedPrimarySource?.kind === 'screen') {
      const cached = screenPreviewCacheRef.current.get(selectedPrimarySource.id);
      if (!cached || !cached.getVideoTracks().some((track) => track.readyState === 'live')) {
        previewTasks.push(attachPreviewStream(selectedPrimarySource, primaryVideoRef.current, 'Program'));
      }
    }
    if (videoFeeds?.layout.pipEnabled && videoFeeds.pip?.kind === 'screen') {
      const cached = screenPreviewCacheRef.current.get(videoFeeds.pip.id);
      if (!cached || !cached.getVideoTracks().some((track) => track.readyState === 'live')) {
        previewTasks.push(attachPreviewStream(videoFeeds.pip, pipVideoRef.current, 'In-screen'));
      }
    }
    for (const task of previewTasks) {
      await task;
    }
  }, [attachPreviewStream, pushLog, selectedPrimarySource, videoFeeds]);

  const toggleControlSection = useCallback((id: ControlSectionId) => {
    setOpenControlSection((current) => (current === id ? null : id));
  }, []);

  const selectedCameraSummary = selectedPrimarySource?.label ?? `${devices.video.length} video`;
  const selectedMicSummary = selectedMic !== '0' ? selectedMic : `${devices.audio.length} audio`;
  const inputsSummary = `${selectedCameraSummary}${videoFeeds?.layout.pipEnabled ? ' + PiP' : ''} / ${selectedMicSummary}`;
  const destinationSummary = selectedManualDestination?.label ?? 'Setup';
  const enabledAudioFilterCount = Number(audioFilters.noiseSuppression) + Number(audioFilters.compressor) + Number(audioFilters.noiseGate);
  const audioSummary = `${Math.round(micLevel * 100)}% / ${enabledAudioFilterCount} filters`;
  const videoSummary = depthOfField && videoCorrection.enabled ? 'DOF + Color' : depthOfField ? 'DOF' : videoCorrection.enabled ? 'Color' : 'Standard';
  const recordingSummary = `${encoder.width}x${encoder.height} / ${recording.compactAfterStop ? 'HEVC' : 'MP4'}`;
  const overlaysSummary = `${visibleOverlays.length} visible`;

  return (
    <div className="studio-shell">
      <aside className="left-rail" aria-label="Source and destination controls">
        <header className="brand-lockup">
          <img src={BRAND_LOGO_URL} alt="" />
          <div>
            <h1>StageBadger</h1>
            <span>Local broadcast studio</span>
          </div>
        </header>

        <ControlAccordionSection
          id="inputs"
          title="Inputs"
          summary={inputsSummary}
          openSection={openControlSection}
          onToggle={toggleControlSection}
        >
          <button className="mini-button" type="button" onClick={refreshDevices}>Refresh devices</button>
          <label>
            Program feed
            <select value={selectedPrimarySource?.id ?? ''} onChange={(event) => setSelectedPrimarySourceId(event.target.value)}>
              {cameraSources.length ? (
                <optgroup label="Cameras">
                  {cameraSources.map((source) => <option key={source.id} value={source.id}>{source.label}</option>)}
                </optgroup>
              ) : null}
              {screenSources.length ? (
                <optgroup label="Screens">
                  {screenSources.map((source) => <option key={source.id} value={source.id}>{source.label}</option>)}
                </optgroup>
              ) : null}
            </select>
          </label>
          <label className="switch-row">
            <input
              type="checkbox"
              checked={pipEnabled}
              disabled={pipCandidates.length === 0}
              onChange={(event) => setPipEnabled(event.target.checked)}
            />
            In-screen
          </label>
          {pipEnabled ? (
            <>
              <label>
                In-screen feed
                <select value={selectedPipSource?.id ?? ''} onChange={(event) => setSelectedPipSourceId(event.target.value)}>
                  {pipCandidates.filter((source) => source.kind === 'camera').length ? (
                    <optgroup label="Cameras">
                      {pipCandidates.filter((source) => source.kind === 'camera').map((source) => (
                        <option key={source.id} value={source.id}>{source.label}</option>
                      ))}
                    </optgroup>
                  ) : null}
                  {pipCandidates.filter((source) => source.kind === 'screen').length ? (
                    <optgroup label="Screens">
                      {pipCandidates.filter((source) => source.kind === 'screen').map((source) => (
                        <option key={source.id} value={source.id}>{source.label}</option>
                      ))}
                    </optgroup>
                  ) : null}
                </select>
              </label>
              <div className="position-chips" role="group" aria-label="In-screen position">
                {([
                  ['bottomRight', 'BR'],
                  ['bottomLeft', 'BL'],
                  ['topRight', 'TR'],
                  ['topLeft', 'TL']
                ] as [PipPosition, string][]).map(([position, label]) => (
                  <button
                    key={position}
                    type="button"
                    className={pipPosition === position ? 'active' : ''}
                    onClick={() => setPipPosition(position)}
                  >
                    {label}
                  </button>
                ))}
              </div>
              <label>
                Size ({pipSizePercent}%)
                <input
                  type="range"
                  min="18"
                  max="35"
                  step="1"
                  value={pipSizePercent}
                  onChange={(event) => setPipSizePercent(Number(event.target.value))}
                />
              </label>
            </>
          ) : null}
          <label>
            Microphone
            <select value={selectedMic} onChange={(event) => setSelectedMic(event.target.value)}>
              {devices.audio.map((mic) => <option key={mic} value={mic}>{mic}</option>)}
            </select>
          </label>
        </ControlAccordionSection>

        <ControlAccordionSection
          id="destination"
          title="Destination"
          summary={destinationSummary}
          summaryOk={Boolean(selectedManualDestination)}
          openSection={openControlSection}
          onToggle={toggleControlSection}
        >
          <div className="destination-wizard">
            <div className="wizard-actions">
              <button type="button" className="secondary-button" onClick={openYoutubeLiveControlRoom}>Open YouTube Live Control Room</button>
              <button type="button" className="secondary-button" onClick={pasteStreamDetails}>Paste from Clipboard</button>
            </div>
            <label>
              Label
              <input value={manualDestinationLabel} onChange={(event) => setManualDestinationLabel(event.target.value)} />
            </label>
            <label>
              Server URL
              <input value={manualServerUrl} onChange={(event) => setManualServerUrl(event.target.value)} />
            </label>
            <label>
              Stream key
              <input
                type="password"
                value={manualStreamKey}
                placeholder="xxxx-xxxx-xxxx"
                onChange={(event) => {
                  setManualStreamKey(event.target.value);
                  setDestinationTest(null);
                }}
              />
            </label>
            <label>
              Privacy note
              <input value={manualPrivacyNote} onChange={(event) => setManualPrivacyNote(event.target.value)} />
            </label>
            <label className="switch-row">
              <input
                type="checkbox"
                checked={manualConfirmedLiveEnabled}
                onChange={(event) => setManualConfirmedLiveEnabled(event.target.checked)}
              />
              I confirmed YouTube Live is enabled
            </label>
            <div className="wizard-actions">
              <button type="button" className="secondary-button" onClick={testManualDestination}>Test destination</button>
              <button type="button" className="secondary-button" onClick={saveManualDestination}>Save destination</button>
            </div>
            <p className="muted">
              Create the stream in YouTube Studio, copy the stream URL and stream key, then save them here. Keys are stored in the OS keychain.
            </p>
            {destinationTest ? (
              <div className={`destination-test ${destinationTest.ok ? 'ok' : 'error'}`} role="status">
                <strong>{destinationTest.ok ? 'Ready' : 'Needs attention'}</strong>
                <span>{destinationTest.message}</span>
                {destinationTest.redactedUrl ? <code>{destinationTest.redactedUrl}</code> : null}
              </div>
            ) : null}
            {manualDestinations.length > 0 ? (
              <div className="destination-list" aria-label="Saved destinations">
                {manualDestinations.map((destination) => (
                  <article key={destination.id} className={destination.id === manualDestinationId ? 'destination-row active' : 'destination-row'}>
                    <button type="button" className="destination-select" onClick={() => selectManualDestination(destination)}>
                      <strong>{destination.label}</strong>
                      <span>{destination.provider === 'youtube' ? 'YouTube RTMPS' : 'Custom RTMP'}</span>
                      <span>{destination.hasSavedKey ? 'Keychain key saved' : 'Missing keychain key'}</span>
                    </button>
                    <button type="button" className="mini-button" onClick={() => deleteManualDestination(destination.id)}>Delete</button>
                  </article>
                ))}
              </div>
            ) : null}
          </div>
          <details className="advanced-block">
            <summary>Advanced / experimental OAuth</summary>
            <div className="stack">
              <label>
                Broadcast title
                <input value={broadcastTitle} onChange={(event) => setBroadcastTitle(event.target.value)} />
              </label>
              <label>
                Privacy
                <select value={privacyStatus} onChange={(event) => setPrivacyStatus(event.target.value)}>
                  <option value="unlisted">Unlisted</option>
                  <option value="private">Private</option>
                  <option value="public">Public</option>
                </select>
              </label>
              <button className="secondary-button" type="button" onClick={connectYoutube}>Connect YouTube</button>
              <p className="muted">{youtubeStatus?.message ?? 'OAuth-managed YouTube remains available behind this panel.'}</p>
            </div>
          </details>
        </ControlAccordionSection>

        <ControlAccordionSection
          id="audio"
          title="Audio"
          summary={audioSummary}
          openSection={openControlSection}
          onToggle={toggleControlSection}
        >
          <div className="vu-meter" aria-label="Microphone level">
            <div className="vu-meter-fill" style={{ width: `${Math.min(micLevel * 100, 100)}%`, background: micLevel > 0.85 ? '#ef4444' : micLevel > 0.6 ? '#f59e0b' : '#22c55e' }} />
          </div>
          <label>
            Gain ({audioFilters.gainDb > 0 ? '+' : ''}{audioFilters.gainDb.toFixed(0)} dB)
            <input type="range" min="-20" max="20" step="1" value={audioFilters.gainDb} onChange={(e) => setAudioFilters((af) => ({ ...af, gainDb: Number(e.target.value) }))} />
          </label>
          <label className="switch-row">
            <input type="checkbox" checked={audioFilters.noiseSuppression} onChange={(e) => setAudioFilters((af) => ({ ...af, noiseSuppression: e.target.checked }))} />
            Noise suppression
          </label>
          <label className="switch-row">
            <input type="checkbox" checked={audioFilters.compressor} onChange={(e) => setAudioFilters((af) => ({ ...af, compressor: e.target.checked }))} />
            Compressor
          </label>
          <label className="switch-row">
            <input type="checkbox" checked={audioFilters.noiseGate} onChange={(e) => setAudioFilters((af) => ({ ...af, noiseGate: e.target.checked }))} />
            Noise gate ({audioFilters.noiseGateThresholdDb} dB)
          </label>
        </ControlAccordionSection>

        <ControlAccordionSection
          id="video"
          title="Video"
          summary={videoSummary}
          openSection={openControlSection}
          onToggle={toggleControlSection}
        >
          <label className="switch-row">
            <input
              type="checkbox"
              checked={depthOfField}
              onChange={(event) => setDepthOfField(event.target.checked)}
            />
            Native depth of field
          </label>
          <label className="switch-row">
            <input type="checkbox" checked={videoCorrection.enabled} onChange={(e) => setVideoCorrection((vc) => ({ ...vc, enabled: e.target.checked }))} />
            Enable color correction
          </label>
          {videoCorrection.enabled ? (
            <>
              <label>Brightness ({videoCorrection.brightness.toFixed(2)})
                <input type="range" min="-1" max="1" step="0.05" value={videoCorrection.brightness} onChange={(e) => setVideoCorrection((vc) => ({ ...vc, brightness: Number(e.target.value) }))} />
              </label>
              <label>Contrast ({videoCorrection.contrast.toFixed(2)})
                <input type="range" min="0" max="3" step="0.05" value={videoCorrection.contrast} onChange={(e) => setVideoCorrection((vc) => ({ ...vc, contrast: Number(e.target.value) }))} />
              </label>
              <label>Saturation ({videoCorrection.saturation.toFixed(2)})
                <input type="range" min="0" max="3" step="0.05" value={videoCorrection.saturation} onChange={(e) => setVideoCorrection((vc) => ({ ...vc, saturation: Number(e.target.value) }))} />
              </label>
              <label>Gamma ({videoCorrection.gamma.toFixed(2)})
                <input type="range" min="0.1" max="5" step="0.1" value={videoCorrection.gamma} onChange={(e) => setVideoCorrection((vc) => ({ ...vc, gamma: Number(e.target.value) }))} />
              </label>
            </>
          ) : null}
        </ControlAccordionSection>

        <ControlAccordionSection
          id="recording"
          title="Recording"
          summary={recordingSummary}
          openSection={openControlSection}
          onToggle={toggleControlSection}
        >
          <label>
            Folder
            <input
              value={recording.directory ?? ''}
              placeholder="/Volumes/MOE/StageBadger/Recordings"
              onChange={(event) => setRecording((current) => ({ ...current, directory: event.target.value || null }))}
            />
          </label>
          <label className="switch-row">
            <input
              type="checkbox"
              checked={recording.compactAfterStop}
              onChange={(event) => setRecording((current) => ({ ...current, compactAfterStop: event.target.checked }))}
            />
            Compact to HEVC after stop
          </label>
          <label>
            Video bitrate
            <input
              type="number"
              min="1500"
              step="500"
              value={encoder.videoBitrateKbps}
              onChange={(event) => setEncoder((current) => ({ ...current, videoBitrateKbps: Number(event.target.value) }))}
            />
          </label>
        </ControlAccordionSection>

        <ControlAccordionSection
          id="overlays"
          title="Overlays"
          summary={overlaysSummary}
          openSection={openControlSection}
          onToggle={toggleControlSection}
        >
          <div className="overlay-grid">
            <button type="button" className={!activeOverlayId ? 'overlay-choice active' : 'overlay-choice'} onClick={() => selectOverlay(null)}>None</button>
            {overlays.map((overlay) => (
              <button
                key={overlay.id}
                type="button"
                className={overlay.visible ? 'overlay-choice active' : 'overlay-choice'}
                onClick={() => selectOverlay(overlay.id)}
              >
                {overlay.name}
              </button>
            ))}
          </div>
          <label className="file-drop">
            Drop PNG, SVG, or WebP
            <input
              type="file"
              accept="image/png,image/svg+xml,image/webp"
              onChange={(event) => {
                const file = event.target.files?.[0];
                if (file) importOverlay(file);
              }}
            />
          </label>
          {activeOverlay ? (
            <div className="overlay-editor">
              <label>Scale <input type="range" min="0.2" max="2" step="0.05" value={activeOverlay.scale} onChange={(event) => updateOverlay({ ...activeOverlay, scale: Number(event.target.value) })} /></label>
              <label>Opacity <input type="range" min="0" max="1" step="0.05" value={activeOverlay.opacity} onChange={(event) => updateOverlay({ ...activeOverlay, opacity: Number(event.target.value) })} /></label>
              <label>X <input type="range" min="0" max="1" step="0.01" value={activeOverlay.x} onChange={(event) => updateOverlay({ ...activeOverlay, x: Number(event.target.value) })} /></label>
              <label>Y <input type="range" min="0" max="1" step="0.01" value={activeOverlay.y} onChange={(event) => updateOverlay({ ...activeOverlay, y: Number(event.target.value) })} /></label>
            </div>
          ) : null}
        </ControlAccordionSection>
      </aside>

      <main className="program-area">
        <header className="top-strip">
          <div>
            <span className={`status-dot ${phase}`}></span>
            <strong>{phaseLabel(phase)}</strong>
            <span>{sessionStatus.destination?.label ?? 'No output active'}</span>
          </div>
          <div className="top-metrics">
            <span>{telemetry.fps?.toFixed(1) ?? '--'} fps</span>
            <span>{telemetry.bitrateKbps ? `${telemetry.bitrateKbps.toFixed(0)} kbps` : '-- kbps'}</span>
            <span>{telemetry.speed ? `${telemetry.speed.toFixed(2)}x` : '-- speed'}</span>
          </div>
        </header>

        {inlineError ? (
          <div className="inline-error" role="alert">
            <strong>Recoverable error</strong>
            <span>{inlineError}</span>
            <button type="button" onClick={() => {
              setInlineError(null);
              if (phase === 'error') setPhase('idle');
            }}>Dismiss</button>
          </div>
        ) : null}

        <section
          className={`preview-stage ${selectedPrimarySource?.kind === 'screen' ? 'screen-primary' : 'camera-primary'}`}
          aria-label="Program preview"
        >
          <video ref={primaryVideoRef} className="program-video primary-video" autoPlay muted playsInline />
          {videoFeeds?.layout.pipEnabled && videoFeeds.pip ? (
            <div className={`pip-frame ${videoFeeds.layout.pipPosition}`} style={{ width: `${videoFeeds.layout.pipSizePercent}%` }}>
              <video ref={pipVideoRef} className={`program-video pip-video ${videoFeeds.pip.kind}`} autoPlay muted playsInline />
            </div>
          ) : null}

          {visibleOverlays.map((overlay) => (
            <img
              key={overlay.id}
              className="program-overlay"
              src={overlaySrc(overlay.assetPath)}
              alt=""
              style={{
                left: `${overlay.x * 100}%`,
                top: `${overlay.y * 100}%`,
                transform: `translate(-50%, -50%) scale(${overlay.scale})`,
                opacity: overlay.opacity,
                zIndex: overlay.zIndex
              }}
            />
          ))}
          <div className="preview-badge">{depthOfField ? 'Native DOF requested' : 'Direct capture'}</div>
          {outputLocked ? <div className="output-locked-pill">Output locked</div> : null}
        </section>

        <footer className="transport">
          <div className="recording-readout">
            <strong>{recordingStatus.path ?? defaultRecordingDirectory}</strong>
            <span>{formatDuration(recordingStatus.durationMs)} · {formatBytes(recordingStatus.bytesWritten)} · {recordingStatus.bitrateKbps ? `${recordingStatus.bitrateKbps.toFixed(0)} kbps` : 'waiting for bitrate'}</span>
            {recordingStatus.compactedPath ? <span>Compact: {formatBytes(recordingStatus.compactedBytes ?? 0)}</span> : null}
          </div>
          <div className="transport-buttons">
            <button type="button" className="secondary-button" disabled={isBusy} onClick={setPreview}>Preview</button>
            <button type="button" className="record-button" disabled={isRunning} onClick={startRecordOnly}>Record</button>
            <button type="button" className="live-button" disabled={isRunning} onClick={startLive}>Go Live</button>
            <button type="button" className="stop-button" disabled={!isRunning && phase !== 'error'} onClick={stopSession}>Stop</button>
          </div>
        </footer>
      </main>

      <aside className="right-rail" aria-label="Transcript chat and telemetry">
        <div className="tabs">
          <button type="button" className={activeTab === 'transcript' ? 'active' : ''} onClick={() => setActiveTab('transcript')}>Transcript</button>
          <button type="button" className={activeTab === 'chat' ? 'active' : ''} onClick={() => setActiveTab('chat')}>Chat</button>
          <button type="button" className={activeTab === 'telemetry' ? 'active' : ''} onClick={() => setActiveTab('telemetry')}>Telemetry</button>
        </div>

        {activeTab === 'transcript' ? (
          <section className="panel-scroll transcript-panel">
            {liveTranscript?.text.trim() ? (
              <article className="transcript-live" aria-live="polite">
                <header className="transcript-segment-header">
                  <span className="segment-range">LIVE</span>
                  <span className="segment-model">{liveTranscript.sourceModel}</span>
                  <span className="segment-chunk">chunk {liveTranscript.chunkId}</span>
                  <span className="segment-confidence">{Math.round(liveTranscript.confidence * 100)}%</span>
                </header>
                <p className="transcript-live-text">{liveTranscript.text}</p>
              </article>
            ) : null}
            {transcript.segments.length === 0 && !liveTranscript?.text.trim() ? <p className="empty-state">Timestamped transcript segments will appear here.</p> : null}
            <div className="transcript-stack">
              {transcript.segments.map((segment) => (
                <article key={segment.id} className="transcript-segment">
                  <header className="transcript-segment-header">
                    <span className="segment-range">{formatTranscriptTimestamp(segment.startMs)} - {formatTranscriptTimestamp(segment.endMs)}</span>
                    <span className="segment-model">{segment.sourceModel}</span>
                    <span className="segment-chunk">chunk {segment.chunkId}</span>
                    <span className="segment-confidence">{Math.round(segment.confidence * 100)}%</span>
                  </header>
                  <p className="segment-words">
                    {segment.words.map((word, index) => (
                      <span
                        key={`${segment.id}-${index}`}
                        className={`word-token ${confidenceClass(word.confidence)}${word.sourceModel !== segment.sourceModel ? ' alternate' : ''}`}
                        title={`${word.sourceModel} · ${formatTranscriptTimestamp(word.startMs)} - ${formatTranscriptTimestamp(word.endMs)}`}
                      >
                        {word.text}
                      </span>
                    ))}
                  </p>
                  {segment.alternates.length > 0 ? (
                    <div className="segment-alternates">
                      {segment.alternates.map((alternate) => (
                        <span key={`${segment.id}-${alternate.modelName}`} className="segment-alternate">
                          {alternate.modelName} {Math.round(alternate.confidence * 100)}%: {alternate.text}
                        </span>
                      ))}
                    </div>
                  ) : null}
                </article>
              ))}
            </div>
          </section>
        ) : null}

        {activeTab === 'chat' ? (
          <section className="panel-scroll chat-panel">
            {chatMessages.length === 0 ? <p className="empty-state">YouTube live chat connects after a broadcast exposes a live chat id.</p> : null}
            {chatMessages.map((message) => (
              <article key={message.id} className={message.isSuperChat ? 'chat-message super' : 'chat-message'}>
                <div>
                  <strong>{message.author}</strong>
                  {message.role ? <span>{message.role}</span> : null}
                  {message.amountDisplay ? <em>{message.amountDisplay}</em> : null}
                </div>
                <p>{message.message}</p>
              </article>
            ))}
          </section>
        ) : null}

        {activeTab === 'telemetry' ? (
          <section className="panel-scroll telemetry-panel">
            <dl>
              <div><dt>Frame</dt><dd>{telemetry.frame ?? '--'}</dd></div>
              <div><dt>Drops</dt><dd>{telemetry.droppedFrames}</dd></div>
              <div><dt>Errors</dt><dd>{telemetry.errors}</dd></div>
              <div><dt>Video engine</dt><dd>{sessionStatus.videoEngine.engine}</dd></div>
              <div><dt>Fallback</dt><dd>{sessionStatus.videoEngine.fallbackActive ? 'active' : 'standby'}</dd></div>
            </dl>
            <div className="log-feed">
              {logs.length === 0 ? <p className="empty-state">System telemetry is idle.</p> : null}
              {logs.map((log) => <p key={log.id}>{log.message}</p>)}
            </div>
          </section>
        ) : null}
      </aside>
    </div>
  );
}
