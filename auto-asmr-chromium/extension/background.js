// 后台（service worker）：管理 offscreen 页面、转发消息、把配置的变化发给正在处理的标签页、显示图标上的角标。
//
// 声音不经过这里。处理流程：弹窗（或快捷键）拿到标签页的 streamId → 这里建好 offscreen 页面并把 streamId 和
// 配置交给它 → offscreen 页面采集、处理、播放。所有标签页都停了就关掉 offscreen 页面，释放内存和声卡。

const OFFSCREEN = 'offscreen.html';
let creating = null;

async function hasOffscreen() {
  const found = await chrome.runtime.getContexts({
    contextTypes: ['OFFSCREEN_DOCUMENT'],
    documentUrls: [chrome.runtime.getURL(OFFSCREEN)],
  });
  return found.length > 0;
}

async function ensureOffscreen() {
  if (await hasOffscreen()) return;
  creating ??= chrome.offscreen
    .createDocument({ url: OFFSCREEN, reasons: ['USER_MEDIA'], justification: '采集标签页的声音，实时处理成双耳 ASMR 后播放' })
    .finally(() => {
      creating = null;
    });
  await creating;
}

async function configText() {
  const { config } = await chrome.storage.local.get('config');
  return typeof config === 'string' ? config : '';
}

function toOffscreen(msg) {
  return chrome.runtime.sendMessage({ to: 'offscreen', ...msg });
}

// 开始处理和"没有标签页了就关掉 offscreen 页面"排队做：最后一个标签页刚停、马上又开始一个时，
// 不会在开始到一半时把 offscreen 页面关掉
let queue = Promise.resolve();
function serial(f) {
  const r = queue.then(f);
  queue = r.catch(() => {});
  return r;
}

/**
 * 开始处理一个标签页。没给 streamId 时在这里取（和 Chrome 官方示例一样由后台取，用户点过插件图标或按过快捷键才取得到）；
 * 取不到时返回 needStreamId，弹窗会自己取一个再发过来。
 */
async function start(tabId, streamId) {
  if (!streamId) {
    try {
      streamId = await chrome.tabCapture.getMediaStreamId({ targetTabId: tabId });
    } catch (e) {
      return { ok: false, needStreamId: true, error: String((e && e.message) || e) };
    }
  }
  return serial(async () => {
    await ensureOffscreen();
    return toOffscreen({ type: 'start', tabId, streamId, config: await configText() });
  });
}

async function stop(tabId) {
  if (!(await hasOffscreen())) return { ok: true };
  return toOffscreen({ type: 'stop', tabId });
}

async function status() {
  if (!(await hasOffscreen())) return { tabs: {}, outputMs: 0, state: 'none' };
  return toOffscreen({ type: 'status' });
}

/** 快捷键：当前标签页在处理就停，没在处理就开始。 */
async function toggle(tab) {
  if (!tab || tab.id === undefined) return;
  const st = await status();
  if (st.tabs[tab.id]) return stop(tab.id);
  const r = await start(tab.id);
  if (r && r.ok === false) console.warn('[auto-asmr] 没开始：', r.error);
  return r;
}

// ------------------------------------------------------------------ 角标

async function activeTabs() {
  const { active } = await chrome.storage.session.get('active');
  return new Set(active || []);
}

async function setActive(tabId, on) {
  const s = await activeTabs();
  if (on) s.add(tabId);
  else s.delete(tabId);
  await chrome.storage.session.set({ active: [...s] });
  await chrome.action.setBadgeText({ tabId, text: on ? 'ON' : '' }).catch(() => {});
}

chrome.runtime.onInstalled.addListener(() => {
  chrome.action.setBadgeBackgroundColor({ color: '#2f6fdd' });
});

// 换页面时 Chrome 可能清掉这个标签页的角标，处理还在继续就补上
chrome.tabs.onUpdated.addListener(async (tabId, info) => {
  if (info.status === 'loading' && (await activeTabs()).has(tabId)) {
    chrome.action.setBadgeText({ tabId, text: 'ON' }).catch(() => {});
  }
});

chrome.tabs.onRemoved.addListener(async (tabId) => {
  const s = await activeTabs();
  if (s.delete(tabId)) await chrome.storage.session.set({ active: [...s] });
});

// ------------------------------------------------------------------ 消息

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg.to !== 'bg') return false;
  (async () => {
    switch (msg.type) {
      // 弹窗发来的
      case 'start':
        return start(msg.tabId, msg.streamId);
      case 'stop':
        return stop(msg.tabId);
      case 'status':
        return status();
      // offscreen 页面发来的
      case 'started':
        await setActive(msg.tabId, true);
        return { ok: true };
      case 'stopped':
        await setActive(msg.tabId, false);
        return { ok: true };
      case 'idle':
        return serial(async () => {
          // 再确认一次（这期间可能又开始了一个）
          const st = await status();
          if (!Object.keys(st.tabs || {}).length && !st.pending && (await hasOffscreen())) await chrome.offscreen.closeDocument();
          return { ok: true };
        });
      default:
        return { ok: false, error: `不认识的消息：${msg.type}` };
    }
  })().then(sendResponse, (e) => sendResponse({ ok: false, error: String((e && e.message) || e) }));
  return true;
});

chrome.commands.onCommand.addListener((command, tab) => {
  if (command === 'toggle') toggle(tab).catch((e) => console.warn('[auto-asmr]', e));
});

// 设置页 / 弹窗改了配置：发给正在处理的标签页
chrome.storage.onChanged.addListener(async (changes, area) => {
  if (area !== 'local' || !changes.config) return;
  if (await hasOffscreen()) toOffscreen({ type: 'config', config: changes.config.newValue || '' }).catch(() => {});
});
