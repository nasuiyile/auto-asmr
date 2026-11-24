// 设置页：全部效果参数（按 schema.js 生成）、风格、效果 / 原声、配置文件的导入导出。
// 改动先写进 wasm 里的配置文档（换算、风格都由 Rust 端的 Params 方法完成），再存进 chrome.storage，
// background.js 把它发给正在处理的标签页。

import { loadAsmr, loadStored, saveStored } from './asmr.js';
import { GROUPS } from './schema.js';

const asmr = await loadAsmr();
let lastText = await loadStored(asmr);
let lists = { pinnas: [], headphones: [] };
try {
  lists = asmr.lists();
} catch (e) {
  console.warn('[auto-asmr] 读不出耳廓 / 耳机列表：', e);
}
let state = asmr.ui();
const rows = [];

const $ = (id) => document.getElementById(id);

function h(tag, props = {}, ...kids) {
  const e = document.createElement(tag);
  Object.assign(e, props);
  for (const k of kids) e.append(k);
  return e;
}

function msg(text, isError = false) {
  const m = $('msg');
  m.textContent = text;
  m.className = isError ? 'error' : 'muted';
}

// ------------------------------------------------------------------ 保存（拖动时最多每 200 ms 存一次，停下后再存一次）

let saveTimer = null;
let savePending = false;
function scheduleSave() {
  savePending = true;
  if (saveTimer) return;
  const run = async () => {
    saveTimer = null;
    if (!savePending) return;
    savePending = false;
    const s = saveStored(asmr);
    lastText = s.text;
    await s.done;
    saveTimer = setTimeout(run, 200);
  };
  saveTimer = setTimeout(run, 0);
}

function change(key, value, src) {
  try {
    asmr.set(key, value);
    msg('');
  } catch (e) {
    msg(e.message, true);
  }
  state = asmr.ui();
  render(src);
  scheduleSave();
}

// ------------------------------------------------------------------ 各种行

const round = (v, dec) => Number(Number(v).toFixed(dec));
const toPos = (d, v) => {
  const t = d.log ? Math.log(v / d.min) / Math.log(d.max / d.min) : (v - d.min) / (d.max - d.min);
  return Math.round(Math.min(1, Math.max(0, t || 0)) * 1000);
};
const fromPos = (d, pos) => {
  const t = pos / 1000;
  return round(d.log ? d.min * Math.pow(d.max / d.min, t) : d.min + t * (d.max - d.min), d.dec);
};
const valueOf = (d, s) => (d.derived ? s.derived[d.key] : d.doc ? s.doc[d.key] : s.params[d.key]);

function labelFor(d) {
  return h('label', { textContent: d.label, title: d.tip || '' });
}

function sliderRow(d) {
  const range = h('input', { type: 'range', min: 0, max: 1000, step: 1, title: d.tip || '' });
  const num = h('input', { type: 'number', step: 'any', title: '可以手输超出滑块范围的值，按 Enter 确认' });
  const note = h('span');
  const row = h('div', { className: 'row' }, labelFor(d), range, h('div', { className: 'val' }, num, note));
  range.addEventListener('input', () => {
    const v = fromPos(d, +range.value);
    num.value = v;
    change(d.key, v, range);
  });
  num.addEventListener('change', () => {
    const v = parseFloat(num.value);
    if (Number.isFinite(v)) change(d.key, v, num);
  });
  return {
    el: row,
    update(s, src) {
      const v = valueOf(d, s);
      if (src !== range) range.value = toPos(d, v);
      if (src !== num) num.value = round(v, d.dec);
      note.textContent = d.fmt ? d.fmt(v) : d.unit || '';
      note.title = note.textContent;
    },
  };
}

function checkRow(d) {
  const box = h('input', { type: 'checkbox' });
  box.addEventListener('change', () => change(d.key, box.checked, box));
  return {
    el: h('label', { className: 'check', title: d.tip || '' }, box, d.label),
    update(s) {
      box.checked = !!s.params[d.key];
    },
  };
}

function choiceRow(d) {
  const btns = d.options.map(([v, text]) => {
    const b = h('button', { textContent: text });
    b.addEventListener('click', () => change(d.key, v, b));
    return [v, b];
  });
  return {
    el: h('div', { className: 'row wide' }, labelFor(d), h('div', { className: 'seg' }, ...btns.map(([, b]) => b))),
    update(s) {
      for (const [v, b] of btns) b.classList.toggle('on', valueOf(d, s) === v);
    },
  };
}

function buttonsRow(d) {
  const btns = d.items.map(([text, key, value, tip]) => {
    const b = h('button', { textContent: text, title: tip || '' });
    b.addEventListener('click', () => change(key, value, b));
    return b;
  });
  return {
    el: h('div', { className: 'row wide' }, h('span'), h('div', { className: 'seg' }, ...btns, h('span', { className: 'muted', textContent: d.note || '' }))),
    update() {},
  };
}

function selectRow(d, options, toValue, fromParams) {
  const sel = h('select', { title: d.tip || '' });
  for (const [v, text] of options) sel.append(h('option', { value: v, textContent: text }));
  sel.addEventListener('change', () => change(d.key, toValue(sel.value), sel));
  return {
    el: h('div', { className: 'row wide' }, labelFor(d), sel),
    update(s) {
      const v = fromParams(s.params[d.key]);
      if (![...sel.options].some((o) => o.value === v)) sel.append(h('option', { value: v, textContent: v }));
      sel.value = v;
    },
  };
}

function pinnaRow(d) {
  const opts = [['KU100', 'KU100（人工头）'], ...lists.pinnas.map((n) => [n, `真人 ${n}`])];
  return selectRow(d, opts, (v) => v, (v) => v);
}

function playbackRow(d) {
  const opts = [['Diffuse', '通用扩散场'], ['Native', 'KU100 原机音色'], ...lists.headphones.map((n) => [`hp:${n}`, n])];
  return selectRow(
    d,
    opts,
    (v) => (v.startsWith('hp:') ? { Headphone: v.slice(3) } : v),
    (p) => (typeof p === 'string' ? p : `hp:${p.Headphone}`),
  );
}

function intRow(d) {
  const num = h('input', { type: 'number', step: 1, min: 0 });
  num.addEventListener('change', () => {
    const v = parseInt(num.value, 10);
    if (Number.isFinite(v) && v >= 0) change(d.key, v, num);
  });
  return {
    el: h('div', { className: 'row wide' }, labelFor(d), h('div', { className: 'val' }, num)),
    update(s, src) {
      if (src !== num) num.value = s.params[d.key];
    },
  };
}

const BUILDERS = { check: checkRow, choice: choiceRow, buttons: buttonsRow, pinna: pinnaRow, playback: playbackRow, int: intRow };

function build() {
  const box = $('groups');
  for (const g of GROUPS) {
    const det = h('details', { open: true }, h('summary', { textContent: g.title }));
    if (g.title.startsWith('入耳')) det.open = state.params.inear_cm > 0;
    if (g.title.startsWith('背景音')) det.open = state.params.bg_level > 0;
    if (g.title === '左右双人') det.open = state.doc.duo_ms > 0;
    if (g.title.startsWith('听歌')) det.open = state.params.stereo_width > 0 || state.params.tone > 0;
    for (const d of g.rows) {
      const r = (BUILDERS[d.type] || sliderRow)(d);
      r.def = d;
      rows.push(r);
      det.append(r.el);
    }
    box.append(det);
  }
}

function render(src) {
  const p = state.params;
  for (const r of rows) {
    const d = r.def;
    r.el.hidden = !!(d.show && !d.show(p, state));
    r.el.classList.toggle('off', !!(d.on && !d.on(p, state)));
    r.update(state, src);
  }
  for (const b of document.querySelectorAll('#dry button')) b.classList.toggle('on', String(state.dry) === b.dataset.dry);
  // 选中的风格（参数和它一致）亮起来；手动调过风格管的参数后都不亮
  for (const b of document.querySelectorAll('[data-style]')) b.classList.toggle('on', state.derived.style === +b.dataset.style);
}

// ------------------------------------------------------------------ 顶部

for (const b of document.querySelectorAll('#dry button')) b.addEventListener('click', () => change('dry', b.dataset.dry === 'true'));
for (const b of document.querySelectorAll('[data-style]')) b.addEventListener('click', () => change('style', +b.dataset.style));

$('import').addEventListener('click', () => $('file').click());
$('file').addEventListener('change', async () => {
  const f = $('file').files[0];
  $('file').value = '';
  if (!f) return;
  try {
    asmr.load(await f.text());
  } catch (e) {
    msg(`没导入：${e.message}`, true);
    return;
  }
  state = asmr.ui();
  render();
  const s = saveStored(asmr);
  lastText = s.text;
  await s.done;
  msg(`已导入 ${f.name}`);
});

$('export').addEventListener('click', () => {
  const url = URL.createObjectURL(new Blob([asmr.text()], { type: 'text/plain' }));
  const a = h('a', { href: url, download: 'auto-asmr-export.ron' });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10000);
  msg('已导出 auto-asmr-export.ron（可以导入桌面版）');
});

$('reset').addEventListener('click', () => {
  if (!confirm('全部效果参数恢复默认？桌面版专用的设置不受影响。')) return;
  asmr.resetParams();
  state = asmr.ui();
  render();
  scheduleSave();
  msg('参数已恢复默认');
});

// 弹窗那边改了：重新读一遍
chrome.storage.onChanged.addListener((changes, area) => {
  if (area !== 'local' || !changes.config) return;
  const text = changes.config.newValue || '';
  if (text === lastText) return;
  lastText = text;
  try {
    if (text) asmr.load(text);
    else asmr.resetParams();
  } catch (e) {
    msg(e.message, true);
  }
  state = asmr.ui();
  render();
});

build();
render();
