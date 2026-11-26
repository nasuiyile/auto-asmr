// 离线渲染测试：和 offscreen.js 一样把 wasm 模块经 processorOptions 交给 worklet.js，输入换成合成的人声，
// 检查输出。结果显示在页面上，最后一行是「全部通过」或「有 N 项失败」；window.testResult 里有详细数据。

const FS = 48000;
const logEl = document.getElementById('log');
const results = [];
const enc = new TextEncoder();
const dec = new TextDecoder();

function log(text, cls = '') {
  const s = document.createElement('span');
  s.className = cls;
  s.textContent = text + '\n';
  logEl.append(s);
}

function check(name, ok, detail = '') {
  results.push({ name, ok, detail });
  log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  — ' + detail : ''}`, ok ? 'pass' : 'fail');
}

/** 合成的"说话"（同 wasm/src/lib.rs 的测试）：按音节开合、偶尔停顿；右声道是左声道的 0.9 倍。 */
function speech(secs) {
  const n = secs * FS;
  const l = new Float32Array(n);
  const r = new Float32Array(n);
  const TAU = 2 * Math.PI;
  for (let i = 0; i < n; i++) {
    const t = i / FS;
    const syl = Math.max(0, Math.sin(TAU * 3.3 * t)) * (Math.sin(TAU * 0.25 * t) > -0.6 ? 1 : 0);
    // 基频 140 ± 20 Hz（相位取它的积分）
    const ph = TAU * (140 * t - (20 / (TAU * 0.7)) * Math.cos(TAU * 0.7 * t));
    const s = 0.2 * syl * (Math.sin(ph) + 0.5 * Math.sin(2 * ph) + 0.3 * Math.sin(3 * ph));
    l[i] = s;
    r[i] = 0.9 * s;
  }
  return [l, r];
}

const wasmBytes = await (await fetch('../extension/asmr.wasm')).arrayBuffer();
const module = await WebAssembly.compile(wasmBytes);
log(`asmr.wasm ${(wasmBytes.byteLength / 1e6).toFixed(1)} MB`);

/** 主线程上的 wasm 实例：生成配置文本、量生成引擎配置的耗时。 */
// 主线程上不能同步实例化超过 8 MB 的模块，要用异步的 instantiate（AudioWorklet 里可以同步）
const ui = (await WebAssembly.instantiate(module, {})).exports;
ui.asmr_init();
function uiCall(f, ...strs) {
  const ptrs = strs.map((s) => {
    const b = enc.encode(s);
    const p = ui.asmr_alloc(b.length);
    new Uint8Array(ui.memory.buffer, p, b.length).set(b);
    return [p, b.length];
  });
  const r = f(...ptrs.flat());
  for (const [p, n] of ptrs) ui.asmr_free(p, n);
  return r;
}
const uiOut = (n) => dec.decode(new Uint8Array(ui.memory.buffer, ui.asmr_out_ptr(), n));
const uiSet = (k, v) => uiCall(ui.asmr_doc_set, k, JSON.stringify(v));
const uiText = () => uiOut(ui.asmr_doc_text());

/**
 * 渲染一段：一个（或几个）处理器，各自接同一段合成人声。`at` = [[秒, node => {...}]]：渲染到那一刻暂停，执行后继续。
 */
async function render({ secs = 10, config = '', gen = 1, nodes = 1, at = [], input = null }) {
  const ctx = new OfflineAudioContext({ numberOfChannels: 2, length: secs * FS, sampleRate: FS });
  await ctx.audioWorklet.addModule('../extension/worklet.js');
  const [l, r] = input || speech(secs);
  const buf = ctx.createBuffer(2, secs * FS, FS);
  buf.copyToChannel(l, 0);
  buf.copyToChannel(r, 1);
  const msgs = [];
  const list = [];
  for (let k = 0; k < nodes; k++) {
    const src = ctx.createBufferSource();
    src.buffer = buf;
    const node = new AudioWorkletNode(ctx, 'auto-asmr', {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      channelCount: 2,
      channelCountMode: 'explicit',
      channelInterpretation: 'speakers',
      processorOptions: { module, config: enc.encode(config), gen },
    });
    node.port.onmessage = (e) => msgs.push({ node: k, ...e.data });
    src.connect(node).connect(ctx.destination);
    src.start();
    list.push(node);
  }
  for (const [t, f] of at) {
    ctx.suspend(t).then(() => {
      f(list[0]);
      // 给消息一点时间送到音频线程
      setTimeout(() => ctx.resume(), 50);
    });
  }
  const t0 = performance.now();
  const out = await ctx.startRendering();
  const ms = performance.now() - t0;
  await new Promise((res) => setTimeout(res, 200));
  return { L: out.getChannelData(0), R: out.getChannelData(1), inL: l, inR: r, msgs, ms };
}

/** 每 0.5 秒一窗的左右电平差（dB），只算有声音的窗。 */
function lrWindows(L, R, from = 2) {
  const w = FS / 2;
  const out = [];
  for (let s = from * FS; s + w <= L.length; s += w) {
    let el = 0;
    let er = 0;
    for (let i = s; i < s + w; i++) {
      el += L[i] * L[i];
      er += R[i] * R[i];
    }
    if (el + er > 1e-4) out.push(10 * Math.log10((el + 1e-12) / (er + 1e-12)));
  }
  return out;
}

function energy(x, from = 0) {
  let e = 0;
  let finite = true;
  for (let i = from; i < x.length; i++) {
    if (!Number.isFinite(x[i])) finite = false;
    e += x[i] * x[i];
  }
  return { e, finite };
}

function maxDiff(a, b, from, lag = 0, gain = 1) {
  let m = 0;
  for (let i = from + lag; i < a.length; i++) m = Math.max(m, Math.abs(a[i] - gain * b[i - lag]));
  return m;
}

// 原声也过峰值限制器（约 2 ms 前瞻）：找出输出比输入晚几个样本（0–256 里和输入差得最少的那个）
function dryLag(a, b, from, gain = 1) {
  let best = 0;
  let bestD = Infinity;
  for (let lag = 0; lag <= 256; lag++) {
    const d = maxDiff(a, b, from, lag, gain);
    if (d < bestD) [best, bestD] = [lag, d];
  }
  return best;
}

const errors = (msgs) => msgs.filter((m) => m.type === 'error').map((m) => (m.detail ? dec.decode(m.detail) : m.message));

// 1. 默认参数（贴耳加强 · 穿梭）
{
  const r = await render({ secs: 12 });
  const el = energy(r.L, 2 * FS);
  const er = energy(r.R, 2 * FS);
  const lr = lrWindows(r.L, r.R);
  const tele = r.msgs.filter((m) => m.type === 'tele');
  const last = tele.length ? tele[tele.length - 1].t : null;
  check('默认参数：有声音、没有 NaN', el.finite && er.finite && el.e + er.e > 1, `能量 ${(el.e + er.e).toFixed(1)}`);
  check('默认参数：左右来回穿梭', Math.max(...lr) > 10 && Math.min(...lr) < -10, `左右差 ${Math.min(...lr).toFixed(1)} … ${Math.max(...lr).toFixed(1)} dB`);
  check('处理器就绪、按时报状态', r.msgs.some((m) => m.type === 'ready') && tele.length >= 40, `状态 ${tele.length} 次，最后：方位 ${last && last[0].toFixed(0)}°，距离 ${last && last[2].toFixed(3)} m，延时 ${last && last[7].toFixed(1)} ms`);
  check('没有报错', !errors(r.msgs).length, errors(r.msgs).join('; '));
  const rt = 12000 / r.ms;
  check('处理速度（离线渲染，含 Web Audio 本身）', rt > 5, `12 秒用了 ${r.ms.toFixed(0)} ms，实时的 ${rt.toFixed(0)} 倍`);
  window.lastRender = r;
}

// 2. 监听原声：输出就是输入（只晚限制器的约 2 ms）
{
  const r = await render({ secs: 4, config: '(dry: true)' });
  const lag = dryLag(r.L, r.inL, 0);
  const d = Math.max(maxDiff(r.L, r.inL, 0, lag), maxDiff(r.R, r.inR, 0, lag));
  check('监听原声：原样输出', d < 1e-6 && lag > 0 && lag < 0.003 * FS, `晚 ${lag} 个样本（${((lag / FS) * 1000).toFixed(1)} ms），最大差 ${d.toExponential(2)}`);
}

// 2b. 监听原声时输出增益照样起作用：−12 dB 后每个样本都是原声的 0.251 倍
{
  const g = Math.pow(10, -12 / 20);
  const r = await render({ secs: 4, config: '(dry: true, params: (out_gain_db: -12.0))' });
  const lag = dryLag(r.L, r.inL, 0, g);
  const d = Math.max(maxDiff(r.L, r.inL, 0, lag, g), maxDiff(r.R, r.inR, 0, lag, g));
  check('监听原声：输出增益起作用', d < 1e-6 && lag > 0, `−12 dB，晚 ${lag} 个样本，和原声 × ${g.toFixed(3)} 的最大差 ${d.toExponential(2)}`);
}

// 3. 处理中途换配置（经 port，同 offscreen.js）：切到原声后原样输出
{
  const r = await render({ secs: 8, at: [[4, (node) => node.port.postMessage({ type: 'config', config: enc.encode('(dry: true)'), gen: 2 })]] });
  const before = lrWindows(r.L.subarray(0, 4 * FS), r.R.subarray(0, 4 * FS), 1.5);
  const from = Math.round(4.5 * FS);
  const lag = dryLag(r.L, r.inL, from);
  const d = Math.max(maxDiff(r.L, r.inL, from, lag), maxDiff(r.R, r.inR, from, lag));
  check('中途切到原声', d < 1e-6 && Math.max(...before.map(Math.abs)) > 6, `切换前左右差最大 ${Math.max(...before.map(Math.abs)).toFixed(1)} dB；切换后与原声（晚 ${lag} 个样本）最大差 ${d.toExponential(2)}`);
}

// 4. 配置读不懂：报 config-error，照常用默认参数处理
{
  const r = await render({ secs: 4, config: 'not ron' });
  const e = energy(r.L, FS);
  const ce = r.msgs.find((m) => m.type === 'config-error');
  check('配置读不懂时报错、照常处理', !!ce && e.e > 0.1 && !errors(r.msgs).length, ce ? dec.decode(ce.detail) : '没有 config-error');
}

// 5. 桌面版格式的配置（由 wasm 生成：掏耳风格 + 雨夜）：声音基本只在一只耳朵里，另一只是雨声
{
  uiSet('style', 2);
  const text = uiText();
  const r = await render({ secs: 10, config: text });
  const lr = lrWindows(r.L, r.R, 3);
  const big = lr.filter((x) => Math.abs(x) > 20).length;
  check('桌面版格式的配置（掏耳）：一只耳朵', text.startsWith('{') && big >= lr.length * 0.8, `${big}/${lr.length} 个窗左右差超过 20 dB（中位 ${lr.sort((a, b) => a - b)[lr.length >> 1].toFixed(0)} dB）`);
  uiSet('bg_preset', 'rain_night');
  const r2 = await render({ secs: 10, config: uiText() });
  // 没雨时静的那只耳朵，加了雨之后应该明显有声音
  const ch = energy(r.L, 3 * FS).e < energy(r.R, 3 * FS).e ? 'L' : 'R';
  const dryQuiet = energy(r[ch], 3 * FS).e;
  const wetQuiet = energy(r2[ch], 3 * FS).e;
  check('雨夜：静的那只耳朵也有雨声', wetQuiet > dryQuiet * 10 && wetQuiet > 1e-3, `${ch} 耳能量 ${wetQuiet.toExponential(2)}（没雨时 ${dryQuiet.toExponential(2)}）`);
  ui.asmr_doc_reset_params();
}

// 5b. 左右双人（桌面版同一个设置）：两个声音一左一右，状态里有 2 号；左右两边差不多一样响；处理中途打开也立即生效
{
  uiSet('duo_ms', 80);
  // 打开时排布切到一起移动（两个声音跟着穿梭、会换边）；这里看各自固定时一左一右的样子（同 wasm/src/lib.rs 的测试）
  uiSet('mix_follow', false);
  const text = uiText();
  const r = await render({ secs: 10, config: text });
  const tele = r.msgs.filter((m) => m.type === 'tele');
  const last = tele.length ? tele[tele.length - 1].t : null;
  const lr = 10 * Math.log10(energy(r.L, 2 * FS).e / energy(r.R, 2 * FS).e);
  check(
    '左右双人：1 号在左、2 号在右，两边差不多一样响',
    !!last && last.length >= 13 && last[8] === 1 && last[0] > 180 && last[9] < 180 && Math.abs(lr) < 6 && !errors(r.msgs).length,
    last ? `1 号 ${last[0].toFixed(0)}°，2 号 ${last[9].toFixed(0)}°，左比右 ${lr.toFixed(1)} dB` : '没有状态',
  );
  uiSet('duo_ms', 0);
  const r2 = await render({ secs: 8, at: [[4, (node) => node.port.postMessage({ type: 'config', config: enc.encode(text), gen: 2 })]] });
  const t2 = r2.msgs.filter((m) => m.type === 'tele');
  const before = t2.find((m) => m.t[8] === 0);
  const after = t2.length ? t2[t2.length - 1].t : null;
  check('左右双人：处理中途打开', !!before && !!after && after[8] === 1 && !errors(r2.msgs).length && energy(r2.L, 5 * FS).finite, after ? `打开后 1 号 ${after[0].toFixed(0)}°，2 号 ${after[9].toFixed(0)}°` : '没有状态');
}

// 5c. 听歌（4 号风格）：左右声道各当一个声源。只在左声道里的声音从左前方来（左耳响几分贝，右耳也听得到），
//     状态里是两个声源（记 2：左声道在左前 45°、右声道在右前 45°）；两个声道一样的内容在正中间（两耳一样响）；
//     处理中途把立体声宽度拖到 0，立即合成一个声源
{
  uiSet('style', 4);
  const text = uiText();
  const [sl] = speech(10);
  const r = await render({ secs: 10, config: text, input: [sl, new Float32Array(sl.length)] });
  const tele = r.msgs.filter((m) => m.type === 'tele');
  const last = tele.length ? tele[tele.length - 1].t : null;
  const side = 10 * Math.log10(energy(r.L, 2 * FS).e / energy(r.R, 2 * FS).e);
  check(
    '听歌：只在左声道的内容从左前方来，状态里是左右两个声源',
    // 合成人声的能量集中在 140–420 Hz，那里左前 45° 的两耳声级差只有两三分贝（频率越高差得越多）
    !!last && last[8] === 2 && Math.abs(last[0] - 315) < 1 && Math.abs(last[9] - 45) < 1 && side > 1.5 && side < 15 && !errors(r.msgs).length,
    last ? `左声道 ${last[0].toFixed(0)}°，右声道 ${last[9].toFixed(0)}°，左耳比右耳响 ${side.toFixed(1)} dB` : '没有状态',
  );
  const c = await render({ secs: 8, config: text, input: [sl.subarray(0, 8 * FS), sl.subarray(0, 8 * FS)] });
  const mid = 10 * Math.log10(energy(c.L, 2 * FS).e / energy(c.R, 2 * FS).e);
  check('听歌：两个声道一样的内容在正中间', Math.abs(mid) < 1.5 && !errors(c.msgs).length, `左比右 ${mid.toFixed(1)} dB`);
  uiSet('stereo_width', 0);
  const mono = uiText();
  const r2 = await render({ secs: 8, config: text, input: [sl.subarray(0, 8 * FS), new Float32Array(8 * FS)], at: [[4, (node) => node.port.postMessage({ type: 'config', config: enc.encode(mono), gen: 2 })]] });
  const t2 = r2.msgs.filter((m) => m.type === 'tele');
  const after = t2.length ? t2[t2.length - 1].t : null;
  const tail = 10 * Math.log10(energy(r2.L, 5 * FS).e / energy(r2.R, 5 * FS).e);
  check('听歌：处理中途把立体声宽度拖到 0', !!after && after[8] === 0 && Math.abs(tail) < 1.5 && !errors(r2.msgs).length, `之后左比右 ${tail.toFixed(1)} dB`);
  ui.asmr_doc_reset_params();
}

// 6. 同一个 AudioContext 里两个标签页（共用一个 wasm 实例）
{
  const r = await render({ secs: 4, nodes: 2 });
  const ready = r.msgs.filter((m) => m.type === 'ready').length;
  const e = energy(r.L, FS).e + energy(r.R, FS).e;
  check('两个处理器同时工作', ready === 2 && e > 0.1 && !errors(r.msgs).length, `就绪 ${ready} 个`);
}

// 6b. 标签页静音（暂停、刷新网页）：说 2 秒后全是 0。输出里不能有非规格化数（wasm 设不了 FTZ / DAZ，x86 上这类数慢几十倍，
//     共用的音频线程会跟不上，所有标签页一起断）；静音 3 秒后不再跑引擎，输出正好是 0
{
  const [l, r] = speech(8);
  l.fill(0, 2 * FS);
  r.fill(0, 2 * FS);
  const res = await render({ secs: 8, input: [l, r] });
  let sub = 0;
  let nonzero = 0;
  for (const ch of [res.L, res.R]) {
    for (let i = 2 * FS; i < ch.length; i++) {
      const a = Math.abs(ch[i]);
      if (a !== 0 && a < 1.1754944e-38) sub++;
      if (i >= 5.2 * FS && a !== 0) nonzero++;
    }
  }
  check('静音时输出里没有非规格化数', sub === 0 && !errors(res.msgs).length, `静音 6 秒里 ${sub} 个`);
  check('静音 3 秒后不跑引擎、输出静音', nonzero === 0 && energy(res.L).e > 0.1, `5.2 秒后非零样本 ${nonzero} 个`);
}

// 7. 改参数的耗时：生成引擎配置 + 处理器换上后的第一块（都在音频线程上做，超过一个渲染块 2.7 ms 就会卡一下）。
//    开始处理时（worklet.js 的 setup）先 warm_up，这里同样先做。
{
  const t = [];
  let worst = 0;
  let t0 = performance.now();
  ui.asmr_warm_up();
  t.push(`warm_up（开始处理时做一次）${(performance.now() - t0).toFixed(1)} ms`);
  ui.asmr_engine_apply();
  const p = ui.asmr_proc_new();
  ui.asmr_proc_process(p, 128);
  const time = (name, f) => {
    f();
    const a = performance.now();
    ui.asmr_engine_apply();
    const b = performance.now();
    ui.asmr_proc_process(p, 128);
    const c = performance.now();
    worst = Math.max(worst, c - a);
    t.push(`${name} ${(b - a).toFixed(1)} + ${(c - b).toFixed(1)} ms`);
  };
  time('参数没变', () => {});
  time('改一个滑块（停留时间）', () => uiSet('hold_s', 3));
  time('换风格 KU100 实录', () => uiSet('style', 1));
  time('换风格 掏耳', () => uiSet('style', 2));
  time('换风格 听歌（第一次：建两个引擎）', () => uiSet('style', 4));
  time('听歌里改立体声宽度', () => uiSet('stereo_width', 60));
  time('换回贴耳加强', () => uiSet('style', 0));
  time('换耳廓', () => uiSet('pinna', 'B'));
  time('换耳机补偿', () => uiSet('playback', 'Diffuse'));
  time('开降噪', () => uiSet('denoise', 100));
  time('开雨声', () => uiSet('bg_preset', 'rain_night'));
  time('改雨量', () => uiSet('rain', 70));
  time('改隔窗', () => uiSet('bg_indoor', 20));
  time('关雨声', () => uiSet('bg_preset', 'off'));
  time('再开雨声', () => uiSet('bg_preset', 'rain_night'));
  ui.asmr_proc_free(p);
  ui.asmr_doc_reset_params();
  log('改参数（生成配置 + 换上后第一块）：' + t.join('，'));
  check('改参数时音频线程上的耗时', worst < 20, `最慢 ${worst.toFixed(1)} ms`);
  window.configCost = t;
}

const failed = results.filter((r) => !r.ok).length;
log(failed ? `有 ${failed} 项失败` : `全部通过（${results.length} 项）`, failed ? 'fail' : 'pass');
window.testResult = { failed, results };
