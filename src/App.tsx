import React, { useState, useEffect, useRef } from 'react';
import { invoke, convertFileSrc } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';

interface AsrToken {
  text: string;
  prob: number;
}

interface AsrResult {
  text: string;
  tokens: AsrToken[];
  confidence: number;
  model_name: string;
}

const OVERLAYS = [
  { name: 'Cyberpunk', path: '/Volumes/MOE/overlays/cyberpunk_stream_overlay_1777494497761.png' },
  { name: 'Corporate', path: '/Volumes/MOE/overlays/minimalist_corporate_overlay_1777494508848.png' },
  { name: 'Gaming', path: '/Volumes/MOE/overlays/gaming_stream_overlay_1777494520635.png' },
  { name: 'None', path: '' }
];

export default function App() {
  const [cameras, setCameras] = useState<string[]>([]);
  const [mics, setMics] = useState<string[]>([]);
  const [selectedCamera, setSelectedCamera] = useState<string>('0');
  const [selectedMic, setSelectedMic] = useState<string>('0');
  const [serverUrl, setServerUrl] = useState('rtmp://a.rtmp.youtube.com/live2/');
  const [youtubeKey, setYoutubeKey] = useState('');
  const [localRecord, setLocalRecord] = useState(true);
  const [activeOverlay, setActiveOverlay] = useState<string>('');
  
  const [isLive, setIsLive] = useState(false);
  const [status, setStatus] = useState<'READY' | 'INITIALIZING' | 'LIVE'>('READY');
  
  // Real-time Tokens
  const [tokensMap, setTokensMap] = useState<{ id: number, tokens: React.ReactNode, model: string }[]>([]);
  const tokenCounter = useRef(0);
  const asrLogRef = useRef<HTMLDivElement>(null);
  const sysLogRef = useRef<HTMLDivElement>(null);
  const cameraPreviewRef = useRef<HTMLVideoElement>(null);

  // System Logs Hook
  const [systemLogs, setSystemLogs] = useState<{ id: number; time: string; msg: string }[]>([]);
  const logCounter = useRef(0);
  
  const addLog = (msg: string) => {
    const time = new Date().toLocaleTimeString();
    setSystemLogs(prev => [...prev, { id: logCounter.current++, time, msg }]);
    if (sysLogRef.current) {
      setTimeout(() => {
        if (sysLogRef.current) sysLogRef.current.scrollTop = sysLogRef.current.scrollHeight;
      }, 50);
    }
  };

  // Initialize Hardware
  const refreshDevices = async () => {
    try {
      addLog("System: Refreshing physical AV interfaces...");
      const devices = await invoke<{ video: string[], audio: string[] }>('get_av_devices');
      setCameras(devices.video);
      setMics(devices.audio);
      addLog(`System: Detected ${devices.video.length} cameras, ${devices.audio.length} microphones`);
    } catch (err) {
      addLog(`Error: Failed refreshing AV devices - ${err}`);
      console.error('Error refreshing AV devices', err);
    }
  };

  useEffect(() => {
    // Start local camera pipeline
    navigator.mediaDevices.getUserMedia({ video: true, audio: false })
      .then(stream => {
        if (cameraPreviewRef.current) {
          cameraPreviewRef.current.srcObject = stream;
        }
      })
      .catch(console.error);

    refreshDevices();

    let unlisten: UnlistenFn | null = null;
    
    // Tauri Event Binding
    const setupListener = async () => {
      unlisten = await listen<AsrResult>('asr_stream', (event) => {
        const data = event.payload;
        if (data.tokens && data.tokens.length > 0) {
          const content = data.tokens.filter(t => !t.text.includes("[_")).map((t, idx) => {
            let confClass = "low";
            if (t.prob > 0.90) confClass = "high";
            else if (t.prob > 0.60) confClass = "med";
            return <span key={idx} className={`token ${confClass}`}>{t.text}</span>;
          });
          
          setTokensMap(prev => [...prev, { id: tokenCounter.current++, tokens: content, model: data.model_name }]);
          
          if (asrLogRef.current) {
            setTimeout(() => {
              if (asrLogRef.current) asrLogRef.current.scrollTop = asrLogRef.current.scrollHeight;
            }, 50);
          }
        }
      });
    };
    
    setupListener();

    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const handleStart = async () => {
    if (!youtubeKey) {
      alert("Error: Please enter a YouTube Stream Key.");
      return;
    }
    
    setStatus('INITIALIZING');
    addLog(`System: Attempting RTMP bound route to: ${serverUrl}`);
    
    try {
      await invoke("start_stream", {
        serverUrl,
        youtubeKey,
        cameraId: selectedCamera,
        micId: selectedMic,
        enableRecording: localRecord,
        overlayPath: activeOverlay
      });
      setIsLive(true);
      setStatus('LIVE');
      addLog(`Success: Hardware Audio Pipeline and FFmpeg streaming LIVE.`);
    } catch (err) {
      addLog(`Error: Stream startup failed: ${err}`);
      alert("Failed to start stream: " + err);
      setStatus('READY');
    }
  };

  const handleStop = async () => {
    try {
      addLog(`System: Issuing Stop Broadcast hook...`);
      await invoke("stop_stream");
      setIsLive(false);
      setStatus('READY');
      addLog(`Success: Broadcast gracefully terminated.`);
    } catch (err) {
      addLog(`Error: Failed dropping broadcast - ${err}`);
      alert("Error stopping: " + err);
    }
  };

  return (
    <div className="app-layout">
      {/* 20-30% Left Pane: Settings & Controls */}
      <aside className="sidebar glass-panel">
        <header className="app-header">
          <h1>🦡 StageBadger</h1>
          <span className="badge" style={{ color: status === 'LIVE' ? 'red' : status === 'INITIALIZING' ? 'yellow' : 'var(--success)' }}>
            {status}
          </span>
        </header>

        <section className="control-section">
          <h3>AV Routing</h3>
          <div className="form-group">
            <label>Camera Source</label>
            <select value={selectedCamera} onChange={e => setSelectedCamera(e.target.value)}>
              {cameras.length === 0 ? <option value="0">Detecting...</option> : cameras.map((c, i) => <option key={i} value={c}>{c}</option>)}
            </select>
          </div>
          <div className="form-group">
            <label>Microphone Source</label>
            <select value={selectedMic} onChange={e => setSelectedMic(e.target.value)}>
              {mics.length === 0 ? <option value="0">Detecting...</option> : mics.map((m, i) => <option key={i} value={m}>{m}</option>)}
            </select>
          </div>
          <button onClick={refreshDevices} className="btn btn-secondary btn-sm">Refresh Hardware</button>
        </section>

        <section className="control-section">
          <h3>Destinations</h3>
          <div className="form-group">
            <label>RTMP Server</label>
            <input type="text" value={serverUrl} onChange={e => setServerUrl(e.target.value)} />
          </div>
          <div className="form-group">
            <label>Stream Key</label>
            <input type="password" placeholder="xxxx-xxxx-xxxx-xxxx" value={youtubeKey} onChange={e => setYoutubeKey(e.target.value)} />
          </div>
          <div className="checkbox-row">
            <input type="checkbox" id="local-record" checked={localRecord} onChange={e => setLocalRecord(e.target.checked)} />
            <label htmlFor="local-record">Save /Volumes/MOE</label>
          </div>
        </section>

        <section className="control-section">
          <h3>UI Overlays</h3>
          <p className="section-desc">Click to load transparent graphical frame over video.</p>
          <div className="overlay-gallery">
            {OVERLAYS.map((o, idx) => (
              <div 
                key={idx} 
                className={`gallery-item ${activeOverlay === o.path ? 'active' : ''}`}
                onClick={() => {
                  addLog(`UI: Swapping Overlay Graphic to [${o.name}]`);
                  setActiveOverlay(o.path);
                }}
              >
                <span className="title">{o.name}</span>
              </div>
            ))}
          </div>
        </section>

        <div className="action-footer">
          <button onClick={handleStart} disabled={isLive || status === 'INITIALIZING'} className="btn btn-primary">Start Broadcast</button>
          <button onClick={handleStop} disabled={!isLive} className="btn btn-danger">Stop Broadcast</button>
        </div>
      </aside>

      {/* 70-80% Right Content: Video & ASR Split */}
      <main className="studio-content" style={{ position: 'relative', paddingBottom: '15vh' }}>
        <div className="video-container glass-panel">
          <video ref={cameraPreviewRef} id="camera-preview" autoPlay muted playsInline></video>
          <img id="css-overlay-preview" src={activeOverlay ? convertFileSrc(activeOverlay) : ''} style={{ display: activeOverlay ? 'block' : 'none' }} />
          <div className="mock-chat">Chat routing disabled.</div>
        </div>

        <div className="asr-terminal glass-panel">
          <div className="terminal-header">
            <h3>🔴 Native Whisper Token Stream (MKL/Metal)</h3>
            <span className="confidence-legend">
              <span className="high">■ &gt;90%</span>
              <span className="med">■ &gt;60%</span>
              <span className="low">■ &lt;60%</span>
            </span>
          </div>
          <div className="terminal-log" ref={asrLogRef}>
            {tokensMap.length === 0 && <p className="sys-msg">Awaiting `cpal` + `whisper-rs` audio pipeline injection...</p>}
            {tokensMap.map(entry => (
              <div key={entry.id} style={{ marginBottom: '0.5rem' }}>
                {entry.tokens} <span style={{ fontSize: '0.6rem', color: 'var(--text-muted)' }}>[{entry.model}]</span>
              </div>
            ))}
          </div>
        </div>

        <div className="telemetry-log glass-panel" ref={sysLogRef} style={{ position: 'absolute', bottom: 0, left: 0, right: 0, height: '15vh', overflowY: 'auto', background: 'rgba(0,0,0,0.8)', padding: '0.5rem', fontSize: '0.75rem', fontFamily: 'monospace', zIndex: 100, borderTop: '1px solid rgba(255,255,255,0.1)' }}>
          <div style={{ color: 'var(--success)' }}>-- 🔴 SYSTEM TELEMETRY ACTIVE --</div>
          {systemLogs.map(log => <div key={log.id}><span style={{ color: '#888' }}>[{log.time}]</span> {log.msg}</div>)}
        </div>
      </main>
    </div>
  );
}
