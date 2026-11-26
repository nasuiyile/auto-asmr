#!/usr/bin/env python3
"""端到端测试：把插件装进 Chrome for Testing，用 Chrome 调试协议（CDP）驱动，真的采集一个标签页的声音。

    python3 tools/e2e_test.py --chrome "<Chrome for Testing 的可执行文件>" [--headful] [--audible]

先 ./build.sh。Chrome for Testing 从 https://googlechromelabs.github.io/chrome-for-testing/ 下载（品牌版 Chrome 137 起
不再支持 --load-extension）。用临时的用户目录，不碰平时用的浏览器。

用户手势（点插件图标）没法自动化：用 Chrome 的测试开关 --allowlisted-extension-id 让这个插件不点图标也能采集，
再在后台（service worker）里直接调用 start() / stop()；弹窗用 chrome.action.openPopup() 打开（打开就自动开始），再拨开关。
真点图标、按快捷键时的手势要求要手动验证。

默认 --headless=new、不出声：发声页等开始采集后才播放（被采集的标签页不在本地出声），插件 offscreen 页面的
AudioContext 换成"无输出设备"。不能用 --mute-audio：它会让标签页采集到的也是静音。只用 Python 标准库。
"""

import argparse
import base64
import hashlib
import http.server
import importlib.util
import json
import math
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, ".."))
EXT = os.path.join(ROOT, "extension")


# ------------------------------------------------------------------ 最小的 websocket 客户端（CDP 用）


class WebSocket:
    def __init__(self, url):
        assert url.startswith("ws://")
        hostport, path = url[5:].split("/", 1)
        host, port = hostport.rsplit(":", 1)
        self.s = socket.create_connection((host, int(port)), timeout=60)
        key = base64.b64encode(os.urandom(16)).decode()
        req = f"GET /{path} HTTP/1.1\r\nHost: {hostport}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        self.s.sendall(req.encode())
        resp = b""
        while b"\r\n\r\n" not in resp:
            chunk = self.s.recv(1)
            if not chunk:
                raise ConnectionError("握手时连接断开")
            resp += chunk
        if b" 101 " not in resp.split(b"\r\n", 1)[0]:
            raise ConnectionError(resp.decode(errors="replace"))

    def _read(self, n):
        out = b""
        while len(out) < n:
            chunk = self.s.recv(n - len(out))
            if not chunk:
                raise ConnectionError("连接断开")
            out += chunk
        return out

    def send(self, text):
        data = text.encode()
        hdr = bytearray([0x81])
        n = len(data)
        if n < 126:
            hdr.append(0x80 | n)
        elif n < 65536:
            hdr.append(0x80 | 126)
            hdr += struct.pack(">H", n)
        else:
            hdr.append(0x80 | 127)
            hdr += struct.pack(">Q", n)
        mask = os.urandom(4)
        hdr += mask
        self.s.sendall(bytes(hdr) + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def recv(self):
        msg = b""
        while True:
            b1, b2 = self._read(2)
            op = b1 & 0x0F
            n = b2 & 0x7F
            if n == 126:
                n = struct.unpack(">H", self._read(2))[0]
            elif n == 127:
                n = struct.unpack(">Q", self._read(8))[0]
            mask = self._read(4) if b2 & 0x80 else None
            payload = self._read(n)
            if mask:
                payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            if op == 0x9:  # ping
                continue
            if op == 0x8:
                raise ConnectionError("CDP 连接关闭")
            msg += payload
            if b1 & 0x80:
                return msg.decode()


class CDP:
    def __init__(self, url):
        self.ws = WebSocket(url)
        self.next_id = 0
        self.events = []

    def call(self, method, params=None, session=None):
        self.next_id += 1
        mid = self.next_id
        m = {"id": mid, "method": method, "params": params or {}}
        if session:
            m["sessionId"] = session
        self.ws.send(json.dumps(m))
        while True:
            r = json.loads(self.ws.recv())
            if r.get("id") == mid:
                if "error" in r:
                    raise RuntimeError(f"{method}: {r['error']}")
                return r.get("result", {})
            self.events.append(r)

    def attach(self, target_id):
        sid = self.call("Target.attachToTarget", {"targetId": target_id, "flatten": True})["sessionId"]
        self.call("Runtime.enable", session=sid)
        return sid

    def eval(self, sid, expr, timeout=30):
        r = self.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True, "timeout": timeout * 1000}, session=sid)
        if "exceptionDetails" in r:
            d = r["exceptionDetails"]
            raise RuntimeError(d.get("exception", {}).get("description") or d.get("text"))
        return r["result"].get("value")

    def targets(self):
        return self.call("Target.getTargets")["targetInfos"]

    def wait_target(self, pred, what, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            for t in self.targets():
                if pred(t):
                    return t
            time.sleep(0.2)
        raise TimeoutError(f"等不到 {what}")

    def errors(self):
        """收集到的页面异常和 console.error。"""
        out = []
        for e in self.events:
            if e.get("method") == "Runtime.exceptionThrown":
                d = e["params"]["exceptionDetails"]
                out.append(d.get("exception", {}).get("description") or d.get("text"))
            elif e.get("method") == "Runtime.consoleAPICalled" and e["params"]["type"] == "error":
                out.append(" ".join(str(a.get("value", a.get("description", ""))) for a in e["params"]["args"]))
        return out


# ------------------------------------------------------------------ 测试


def ext_id_for(path):
    """未打包插件的 ID：路径的 SHA-256 前 16 字节，十六进制的每一位换成 a–p。"""
    h = hashlib.sha256(path.encode()).hexdigest()[:32]
    return "".join(chr(ord("a") + int(c, 16)) for c in h)


results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok)))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  — {detail}" if detail else ""), flush=True)


def serve():
    spec = importlib.util.spec_from_file_location("serve", os.path.join(HERE, "serve.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)

    class Quiet(mod.Handler):
        def log_message(self, *a):
            pass

    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Quiet)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv.server_address[1]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--chrome", required=True)
    ap.add_argument("--headful", action="store_true", help="显示浏览器窗口")
    ap.add_argument("--audible", action="store_true", help="处理后的声音接到声卡（默认不出声）")
    ap.add_argument("--offscreen-window", action="store_true", help="和 --headful 一起用：窗口放到屏幕外")
    args = ap.parse_args()
    if not os.path.isfile(os.path.join(EXT, "asmr.wasm")):
        sys.exit("先运行 ./build.sh")

    ext_id = ext_id_for(EXT)
    port = serve()
    page = f"http://127.0.0.1:{port}/tools/e2e-page.html"
    profile = tempfile.mkdtemp(prefix="asmr-e2e-")
    cmd = [
        args.chrome,
        f"--user-data-dir={profile}",
        "--remote-debugging-port=0",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-search-engine-choice-screen",
        f"--load-extension={EXT}",
        f"--disable-extensions-except={EXT}",
        f"--allowlisted-extension-id={ext_id}",
        "--autoplay-policy=no-user-gesture-required",
        "about:blank",
    ]
    if not args.headful:
        cmd.insert(1, "--headless=new")
    elif args.offscreen_window:
        cmd.insert(1, "--window-position=-3000,-3000")
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        port_file = os.path.join(profile, "DevToolsActivePort")
        for _ in range(100):
            if os.path.exists(port_file) and open(port_file).read().strip():
                break
            time.sleep(0.1)
        dport = open(port_file).read().split()[0]
        ver = json.load(urllib.request.urlopen(f"http://127.0.0.1:{dport}/json/version"))
        print(f"{ver['Browser']}，插件 ID {ext_id}", flush=True)
        cdp = CDP(ver["webSocketDebuggerUrl"])
        run(cdp, ext_id, page, args.audible)
    finally:
        proc.terminate()
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(profile, ignore_errors=True)
    failed = sum(1 for _, ok in results if not ok)
    print(f"有 {failed} 项失败" if failed else f"全部通过（{len(results)} 项）")
    sys.exit(1 if failed else 0)


TAP_JS = """
const orig = AudioNode.prototype.connect;
AudioNode.prototype.connect = function (dst, ...rest) {
  const r = orig.call(this, dst, ...rest);
  if (dst instanceof AudioDestinationNode) {
    const sp = this.context.createChannelSplitter(2);
    orig.call(this, sp);
    const an = [0, 1].map((i) => { const a = this.context.createAnalyser(); a.fftSize = 2048; orig.call(sp, a, i); return a; });
    (window.__taps ||= []).push(an);
  }
  return r;
};
true
"""

# 输出端每 0.5 秒一窗的左右电平差（dB；没声音的窗为 null），共 n 窗
LR_JS = """(async (n) => {
  const [l, r] = window.__taps.at(-1);
  const bl = new Float32Array(2048), br = new Float32Array(2048);
  const out = [];
  for (let w = 0; w < n; w++) {
    let el = 0, er = 0;
    for (let k = 0; k < 12; k++) {
      await new Promise((res) => setTimeout(res, 40));
      l.getFloatTimeDomainData(bl); r.getFloatTimeDomainData(br);
      for (let i = 0; i < 2048; i++) { el += bl[i] * bl[i]; er += br[i] * br[i]; }
    }
    out.push(el + er > 1e-4 ? 10 * Math.log10((el + 1e-12) / (er + 1e-12)) : null);
  }
  return out;
})(%d)"""


def multi_tabs(cdp, bg, page, prepare_offscreen, n=6):
    """多个标签页一起处理、反复刷新（B 站这类页面刷新时有一两秒静音）：都照常处理；同一个标签页同时开始两次只算一次；
    最后一个标签页刚停就开始另一个也能开始；全关掉后 offscreen 页面关掉。"""

    def open_tab(k):
        t = cdp.call("Target.createTarget", {"url": page + f"?wait&tab={k}"})["targetId"]
        s = cdp.attach(t)
        time.sleep(0.8)
        return t, s, cdp.eval(bg, "chrome.tabs.query({}).then(ts => Math.max(...ts.map(t => t.id)))")

    def play(s):
        for _ in range(50):
            if cdp.eval(s, "typeof window.play === 'function'"):
                return cdp.eval(s, "window.play()")
            time.sleep(0.1)

    def speaking(tabs, secs=4):
        """几秒内引擎看到在说话（发声页的合成人声有停顿，取最大值）的标签页有几个。"""
        best = {}
        for _ in range(int(secs / 0.25)):
            time.sleep(0.25)
            st = cdp.eval(bg, "status()")
            for _, _, tab in tabs:
                me = st["tabs"].get(str(tab))
                if me and me.get("tele") and not me.get("error"):
                    best[tab] = max(best.get(tab, 0), me["tele"]["act"])
        return sum(1 for v in best.values() if v > 0.3)

    prepare_offscreen()
    tabs = [open_tab(k) for k in range(n)]
    rs = [cdp.eval(bg, f"start({tab})") for _, _, tab in tabs]
    for _, s, _ in tabs:
        play(s)
    ok = speaking(tabs)
    check(f"{n} 个标签页同时处理", all(r.get("ok") for r in rs) and ok == n, f"{ok} 个在说话")
    for k in range(2):
        for _, s, _ in tabs:
            cdp.call("Page.reload", {}, session=s)
        time.sleep(1.5)
        for _, s, _ in tabs:
            play(s)
        ok = speaking(tabs)
        n_tabs = len(cdp.eval(bg, "status()")["tabs"])
        check(f"{n} 个标签页一起刷新（第 {k + 1} 次）后照常处理", ok == n and n_tabs == n, f"{ok} 个在说话，在处理 {n_tabs} 个")

    # 同一个标签页、同一个 streamId 同时发两次开始：只开始一次，两次都回应成功
    for _, s, _ in tabs:
        cdp.eval(s, "document.getElementById('a').pause(); true")
    extra = open_tab(n)
    r = cdp.eval(
        bg,
        f"chrome.tabCapture.getMediaStreamId({{ targetTabId: {extra[2]} }}).then((id) => serial(async () => {{ await ensureOffscreen();"
        f" return Promise.all([1, 2].map(() => toOffscreen({{ type: 'start', tabId: {extra[2]}, streamId: id, config: '' }}))); }}))",
    )
    n_tabs = len(cdp.eval(bg, "status()")["tabs"])
    check("同一个标签页同时开始两次只算一次", all(x.get("ok") for x in r) and n_tabs == n + 1, f"{json.dumps(r, ensure_ascii=False)}，在处理 {n_tabs} 个")

    # 停掉所有标签页，最后一个刚停就开始另一个（offscreen 页面不能在开始到一半时被关掉）
    for _, _, tab in tabs[:-1] + [extra]:
        cdp.eval(bg, f"stop({tab})")
    last = tabs[-1]
    again = tabs[0]
    r = cdp.eval(bg, f"stop({last[2]}).then(() => start({again[2]}))")
    time.sleep(1.5)
    st = cdp.eval(bg, "status()")
    check("最后一个刚停就开始另一个", r.get("ok") and list(st["tabs"]) == [str(again[2])], f"{json.dumps(r, ensure_ascii=False)}，在处理 {list(st['tabs'])}")

    # 关掉所有标签页：自动停止、关掉 offscreen 页面
    for t, _, _ in tabs + [extra]:
        cdp.call("Target.closeTarget", {"targetId": t})
    time.sleep(2)
    n_off = cdp.eval(bg, "chrome.runtime.getContexts({ contextTypes: ['OFFSCREEN_DOCUMENT'] }).then(c => c.length)")
    check("关掉所有标签页后释放 offscreen 页面", n_off == 0, f"offscreen {n_off} 个")


def run(cdp, ext_id, page, audible):
    origin = f"chrome-extension://{ext_id}/"
    sw = cdp.wait_target(lambda t: t["type"] == "service_worker" and t["url"].startswith(origin), "插件的 service worker")
    check("插件已加载（service worker 在运行）", True, sw["url"])
    bg = cdp.attach(sw["targetId"])

    def prepare_offscreen():
        """先建好 offscreen 页面（start() 会复用它）；不出声时把它的 AudioContext 换成"无输出设备"，照常按实时速度处理。"""
        cdp.eval(bg, "ensureOffscreen()")
        off = cdp.wait_target(lambda t: t["url"] == origin + "offscreen.html", "offscreen 页面", 10)
        sid = cdp.attach(off["targetId"])  # 也用来收集它的报错
        if not audible:
            cdp.eval(sid, "window.AudioContext = class extends AudioContext { constructor(o = {}) { super({ ...o, sinkId: { type: 'none' } }); } }; true")
        # 接到输出端的节点再分一路给两个分析器（左、右），用来测处理后的声音
        cdp.eval(sid, TAP_JS)
        return sid

    # 打开发声页（先不播放），拿到它的标签页 ID
    tid = cdp.call("Target.createTarget", {"url": page + "?wait"})["targetId"]
    ps = cdp.attach(tid)
    time.sleep(1)
    # 插件没有 tabs 权限，看不到网址：取最新建的那个标签页（ID 递增）
    tab_id = cdp.eval(bg, "chrome.tabs.query({}).then(ts => Math.max(...ts.map(t => t.id)))")

    # 开始（同快捷键的路径：后台取 streamId、建 offscreen 页面、采集），然后发声页开始播放
    osid = prepare_offscreen()
    r = cdp.eval(bg, f"start({tab_id})")
    check("start() 成功", r and r.get("ok"), json.dumps(r, ensure_ascii=False))
    cdp.eval(ps, "window.play()")
    time.sleep(1)
    play = cdp.eval(ps, "({ t: document.getElementById('a').currentTime, err: window.playError })")
    check("发声页在播放", play["t"] > 0.2 and not play["err"], f"currentTime {play['t']:.2f} {play['err']}")

    az, act, ins, st = [], [], [], None
    for _ in range(20):
        time.sleep(0.25)
        st = cdp.eval(bg, "status()")
        me = st["tabs"].get(str(tab_id))
        if me and me.get("tele"):
            az.append(me["tele"]["az"])
            act.append(me["tele"]["act"])
            ins.append(me["tele"]["inDb"])
    me = st["tabs"].get(str(tab_id)) or {}
    tele = me.get("tele") or {}
    check("AudioContext 在运行", st.get("state") == "running", f"state {st.get('state')}，输出延时 {st.get('outputMs', 0):.1f} ms")
    # 输入电平是会衰减的峰值，停顿里会落下去，取这段时间里的最大值（合成人声的峰值约 −9 dBFS）
    check("采集到了标签页的声音（引擎看到在说话）", tele and max(act) > 0.3 and max(ins) > -20, f"说话最高 {max(act or [0]) * 100:.0f}%，输入峰值最高 {max(ins or [-999]):.1f} dB")
    span = (max(az) - min(az)) if az else 0
    check("声源在左右移动", span > 60, f"5 秒内方位角范围 {span:.0f}°")
    check("没有出错", not me.get("error"), me.get("error", ""))
    badge = cdp.eval(bg, f"chrome.action.getBadgeText({{ tabId: {tab_id} }})")
    check("图标角标显示 ON", badge == "ON", repr(badge))

    # 输出端实测：穿梭时左右电平差来回变；切到「原声」后恒为原片的左右差（右声道是左声道的 0.9 倍 → 0.9 dB）
    cdp.eval(bg, "chrome.storage.local.set({ config: '(params: (hold_s: 1.0, move_s: 1.0))' })")
    lr = [x for x in cdp.eval(osid, LR_JS % 20, timeout=60) if x is not None]
    check("输出端：左右来回（双耳处理过）", lr and max(lr) > 10 and min(lr) < -10, f"左右差 {min(lr or [0]):.1f} … {max(lr or [0]):.1f} dB（{len(lr)} 窗）")
    cdp.eval(bg, "chrome.storage.local.set({ config: '(dry: true)' })")
    time.sleep(1)
    lr = [x for x in cdp.eval(osid, LR_JS % 4) if x is not None]
    want = 20 * math.log10(1 / 0.9)
    check("输出端：切到「原声」后就是原来的立体声", lr and all(abs(x - want) < 0.3 for x in lr), f"左右差 {', '.join(f'{x:.2f}' for x in lr)} dB（原片 {want:.2f} dB）")

    # 改配置（同设置页：写 chrome.storage → 后台转发 → worklet 换上）
    cdp.eval(bg, "chrome.storage.local.set({ config: '(params: (mode: Static, static_az: 0.0, micro: 0.0, sway: 0.0, updown: 0.0))' })")
    time.sleep(2.5)
    t = cdp.eval(bg, "status()")["tabs"][str(tab_id)]["tele"]
    d = min(abs(t["az"]), abs(360 - t["az"]))
    check("改配置后立即生效（固定在正前方）", d < 10, f"方位角 {t['az']:.1f}°")

    # 设置页：在插件环境里（CSP、wasm）能打开，改风格会存成桌面版格式、并传到正在处理的标签页
    opt = cdp.call("Target.createTarget", {"url": origin + "options.html"})["targetId"]
    osid = cdp.attach(opt)
    time.sleep(1.5)
    rows = cdp.eval(osid, "document.querySelectorAll('.row').length")
    check("设置页打开、参数表生成", rows and rows > 50, f"{rows} 行")
    cdp.eval(osid, "[...document.querySelectorAll('[data-style]')].find(b => b.dataset.style === '2').click()")
    time.sleep(1)
    cfg = cdp.eval(bg, "chrome.storage.local.get('config').then(x => x.config)")
    check("设置页点「掏耳」：存成桌面版格式", cfg.startswith("{") and '"app"' in cfg and "inear_cm: 2.1" in cfg, cfg[:40].replace("\n", " "))
    # 声源从正前方移到耳边要几秒（移动跟着说话走），最多等 10 秒，看它贴到耳边、伸进耳道
    best_r, best_depth = 9.0, 0.0
    for _ in range(40):
        time.sleep(0.25)
        t = cdp.eval(bg, "status()")["tabs"][str(tab_id)]["tele"]
        best_r, best_depth = min(best_r, t["r"]), max(best_depth, t["depthCm"])
        if best_r < 0.1 and best_depth > 0.3:
            break
    check("…并传到了正在处理的标签页（贴到耳边、伸进耳道）", best_r < 0.1 and best_depth > 0.3, f"离头中心最近 {best_r * 100:.1f} cm，入耳最深 {best_depth:.2f} cm")
    cdp.call("Target.closeTarget", {"targetId": opt})

    # 弹窗页面当成普通标签页打开时（"当前标签页"是它自己）：能正常显示，不自动开始
    n_before = len(cdp.eval(bg, "status()")["tabs"])
    pop = cdp.call("Target.createTarget", {"url": origin + "popup.html"})["targetId"]
    psid = cdp.attach(pop)
    time.sleep(1.5)
    txt = cdp.eval(psid, "document.getElementById('status').textContent + ' | 开关 ' + document.getElementById('toggle').checked")
    n_after = len(cdp.eval(bg, "status()")["tabs"])
    check("弹窗页面在标签页里打开：正常显示、不自动开始", "没在处理" in txt and "开关 false" in txt and n_after == n_before, txt[:70])
    cdp.call("Target.closeTarget", {"targetId": pop})

    # 标签页里换一个页面：采集继续
    cdp.call("Page.navigate", {"url": page + "?again"}, session=ps)
    time.sleep(1.5)
    # 新页面自己开始播放；合成人声有停顿，几秒内看说话活动的最大值
    acts, alive = [], True
    for _ in range(16):
        time.sleep(0.25)
        me = cdp.eval(bg, "status()")["tabs"].get(str(tab_id))
        alive = alive and bool(me)
        if me and me.get("tele"):
            acts.append(me["tele"]["act"])
    check("标签页换页面后继续处理", alive and acts and max(acts) > 0.3, f"说话最高 {max(acts or [0]) * 100:.0f}%")

    # 停止：offscreen 页面关掉、角标清掉（先暂停发声页，停止采集后它会在本地出声）
    cdp.eval(ps, "document.getElementById('a').pause(); true")
    cdp.eval(bg, f"stop({tab_id})")
    time.sleep(1.5)
    n_off = cdp.eval(bg, "chrome.runtime.getContexts({ contextTypes: ['OFFSCREEN_DOCUMENT'] }).then(c => c.length)")
    badge = cdp.eval(bg, f"chrome.action.getBadgeText({{ tabId: {tab_id} }})")
    check("stop() 后关掉 offscreen 页面、清掉角标", n_off == 0 and badge == "", f"offscreen {n_off} 个，角标 {badge!r}")

    # 再开始，然后直接关掉标签页：自动停止
    prepare_offscreen()
    r = cdp.eval(bg, f"start({tab_id})")
    cdp.eval(ps, "window.play()")
    acts, ins = [], []
    for _ in range(16):
        time.sleep(0.25)
        me = cdp.eval(bg, "status()")["tabs"].get(str(tab_id)) or {}
        if me.get("tele"):
            acts.append(me["tele"]["act"])
            ins.append(me["tele"]["inDb"])
    pl = cdp.eval(ps, "({ t: document.getElementById('a').currentTime, paused: document.getElementById('a').paused, err: window.playError })")
    check("停止后再开始：照常处理", r.get("ok") and acts and max(acts) > 0.3, f"说话最高 {max(acts or [0]) * 100:.0f}%，输入峰值最高 {max(ins or [-999]):.1f} dB，发声页 {pl}")
    cdp.call("Target.closeTarget", {"targetId": tid})
    time.sleep(2)
    n_off = cdp.eval(bg, "chrome.runtime.getContexts({ contextTypes: ['OFFSCREEN_DOCUMENT'] }).then(c => c.length)")
    check("关掉正在处理的标签页后自动停止", r.get("ok") and n_off == 0, f"offscreen {n_off} 个")

    multi_tabs(cdp, bg, page, prepare_offscreen)

    # 真弹窗（chrome.action.openPopup）：打开就自动开始处理当前标签页、显示实时状态；关掉开关停止
    tid = cdp.call("Target.createTarget", {"url": page + "?wait"})["targetId"]
    ps = cdp.attach(tid)
    time.sleep(1)
    prepare_offscreen()
    try:
        cdp.eval(bg, "chrome.action.openPopup().then(() => true)")
        pop = cdp.wait_target(lambda t: t["url"] == origin + "popup.html", "弹窗", 5)
    except Exception as e:  # 有的环境（比如没有获得焦点的窗口）打不开弹窗
        check("弹窗：打开", False, str(e)[:120])
    else:
        psid = cdp.attach(pop["targetId"])
        cdp.eval(ps, "window.play()")
        txt = ""
        for _ in range(20):
            time.sleep(0.25)
            txt = cdp.eval(psid, "document.getElementById('status').textContent + ' | 开关 ' + document.getElementById('toggle').checked")
            if "离头" in txt and "说话 0%" not in txt:
                break
        check("弹窗：打开就自动开始、显示实时状态", "离头" in txt and "开关 true" in txt, txt[:90])
        cdp.eval(ps, "document.getElementById('a').pause(); true")
        cdp.eval(psid, "document.getElementById('toggle').click(); true")
        time.sleep(1.5)
        n_off = cdp.eval(bg, "chrome.runtime.getContexts({ contextTypes: ['OFFSCREEN_DOCUMENT'] }).then(c => c.length)")
        txt = cdp.eval(psid, "document.getElementById('status').textContent + ' | 开关 ' + document.getElementById('toggle').checked")
        check("弹窗：关掉开关后停止", n_off == 0 and "已停止" in txt and "开关 false" in txt, f"offscreen {n_off} 个；{txt[:60]}")
        # 再打开开关：重新开始
        prepare_offscreen()
        cdp.eval(psid, "document.getElementById('toggle').click(); true")
        cdp.eval(ps, "window.play()")
        for _ in range(20):
            time.sleep(0.25)
            txt = cdp.eval(psid, "document.getElementById('status').textContent + ' | 开关 ' + document.getElementById('toggle').checked")
            if "离头" in txt:
                break
        check("弹窗：再打开开关后重新开始", "离头" in txt and "开关 true" in txt, txt[:70])
        cdp.eval(ps, "document.getElementById('a').pause(); true")
        cdp.eval(psid, "document.getElementById('toggle').click(); true")
        time.sleep(1)

    errs = cdp.errors()
    check("各页面没有报错", not errs, "; ".join(errs)[:300])


if __name__ == "__main__":
    main()
