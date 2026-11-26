// 看不见的页面（offscreen document）：拿标签页的声音、交给 AudioWorklet 里的引擎处理、再播出去。
//
// 标签页被采集后，Chrome 不再直接播放它的声音；这里把处理后的声音接到 AudioContext 的输出，听到的就只有处理后的那一份。
// 一个 AudioContext（固定 48 kHz，和引擎内部一致）里可以同时处理几个标签页，每个标签页一个 AudioWorkletNode。
// offscreen 页面只能用 chrome.runtime，配置由 background.js 发过来。

const tabs = new Map(); // tabId → { stream, src, node, tele, error, started }
const starting = new Map(); // tabId → 正在开始的 Promise（同一个标签页同时只开始一次）
let ctx = null;
let wasmModule = null;
let ready = null;
let configBytes = new Uint8Array(0);
let gen = 0;

const dec = new TextDecoder();

async function init() {
  ctx = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
  // 被挂起（换输出设备、系统休眠等）后，还有标签页在处理就接着跑：被采集的标签页只从这里出声
  ctx.onstatechange = () => {
    if (ctx.state === 'suspended' && tabs.size) ctx.resume().catch(() => {});
  };
  await ctx.audioWorklet.addModule('worklet.js');
  try {
    wasmModule = await WebAssembly.compileStreaming(fetch('asmr.wasm'));
  } catch {
    wasmModule = await WebAssembly.compile(await (await fetch('asmr.wasm')).arrayBuffer());
  }
}

function setConfig(text) {
  if (typeof text !== 'string') return;
  configBytes = new TextEncoder().encode(text);
  gen += 1;
  for (const t of tabs.values()) t.node.port.postMessage({ type: 'config', config: configBytes, gen });
}

function notify(msg) {
  chrome.runtime.sendMessage({ to: 'bg', ...msg }).catch(() => {});
}

function start(tabId, streamId, config) {
  if (tabs.has(tabId)) {
    if (ctx.state !== 'running') ctx.resume().catch(() => {});
    return { ok: true };
  }
  // 同一个标签页同时只开始一次（快捷键和弹窗前后脚）。没开始成、也没有别的标签页时通知后台关掉 offscreen 页面
  if (!starting.has(tabId)) {
    const p = open(tabId, streamId, config).finally(() => {
      starting.delete(tabId);
      if (!tabs.size && !starting.size) notify({ type: 'idle' });
    });
    starting.set(tabId, p);
  }
  return starting.get(tabId);
}

async function open(tabId, streamId, config) {
  ready ??= init();
  await ready;
  setConfig(config);
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { mandatory: { chromeMediaSource: 'tab', chromeMediaSourceId: streamId } },
    video: false,
  });
  let t;
  try {
    const src = ctx.createMediaStreamSource(stream);
    const node = new AudioWorkletNode(ctx, 'auto-asmr', {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      channelCount: 2,
      channelCountMode: 'explicit',
      channelInterpretation: 'speakers',
      processorOptions: { module: wasmModule, config: configBytes, gen },
    });
    t = { stream, src, node, tele: null, error: '', started: Date.now() };
    node.port.onmessage = (e) => onWorklet(tabId, t, e.data);
    node.onprocessorerror = () => {
      t.error = '音频处理器出错，已停止';
    };
    src.connect(node);
    node.connect(ctx.destination);
  } catch (e) {
    // 标签页已经被采集、不在本地出声了：这里不停掉采集，它会一直没声音
    for (const track of stream.getTracks()) track.stop();
    throw e;
  }
  for (const track of stream.getAudioTracks()) track.addEventListener('ended', () => stop(tabId));
  tabs.set(tabId, t);
  if (ctx.state !== 'running') await ctx.resume();
  notify({ type: 'started', tabId });
  return { ok: true };
}

function stop(tabId) {
  const t = tabs.get(tabId);
  if (!t) return;
  tabs.delete(tabId);
  for (const track of t.stream.getTracks()) track.stop();
  t.src.disconnect();
  t.node.disconnect();
  t.node.port.postMessage({ type: 'free' });
  notify({ type: 'stopped', tabId });
  if (!tabs.size) notify({ type: 'idle' });
}

function onWorklet(tabId, t, m) {
  if (m.type === 'tele') {
    t.tele = m.t;
  } else if (m.type === 'error') {
    const detail = m.detail ? dec.decode(m.detail) : '';
    t.error = `处理出错，已改成直通：${detail || m.message}`;
    console.error('[auto-asmr]', tabId, m.message, detail);
  } else if (m.type === 'config-error') {
    const detail = m.detail ? dec.decode(m.detail) : '';
    console.warn('[auto-asmr] 配置没换成：', detail);
  }
}

function status() {
  const out = {};
  for (const [id, t] of tabs) {
    const tl = t.tele;
    out[id] = {
      error: t.error,
      seconds: (Date.now() - t.started) / 1000,
      // 有两个声音时 duo 为真：az… 是第一个，az2… 是第二个。左右双人是 1 号、2 号；stereo 为真时是立体声（听歌）的左声道、右声道
      tele: tl && { az: tl[0], el: tl[1], r: tl[2], act: tl[3], inDb: tl[4], outDb: tl[5], depthCm: tl[6], engineMs: tl[7], duo: tl[8] > 0, stereo: tl[8] > 1.5, az2: tl[9], el2: tl[10], r2: tl[11], act2: tl[12] },
    };
  }
  return {
    tabs: out,
    // 正在开始、还没开始好的标签页有几个（这时不能关 offscreen 页面）
    pending: starting.size,
    // 输出端的延时：AudioContext 的缓冲 + 声卡（标签页采集本身的延时 Chrome 不报告，不在这里）
    outputMs: ctx ? ((ctx.baseLatency || 0) + (ctx.outputLatency || 0)) * 1000 : 0,
    state: ctx ? ctx.state : 'none',
  };
}

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.to !== 'offscreen') return false;
  (async () => {
    switch (msg.type) {
      case 'start':
        return start(msg.tabId, msg.streamId, msg.config);
      case 'stop':
        stop(msg.tabId);
        return { ok: true };
      case 'config':
        setConfig(msg.config);
        return { ok: true };
      case 'status':
        return status();
      default:
        return { ok: false, error: `不认识的消息：${msg.type}` };
    }
  })().then(sendResponse, (e) => sendResponse({ ok: false, error: String((e && e.message) || e) }));
  return true;
});
