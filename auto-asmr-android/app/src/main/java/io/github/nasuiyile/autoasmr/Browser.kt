package io.github.nasuiyile.autoasmr

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Bitmap
import android.net.Uri
import android.util.Log
import android.view.View
import android.webkit.ConsoleMessage
import android.webkit.CookieManager
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.webkit.JavaScriptReplyProxy
import androidx.webkit.WebViewAssetLoader
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import org.json.JSONObject

/**
 * WebView：切到后台、锁屏时窗口变成不可见，Chromium 就会暂停网页里的视频。正在出声的那个标签页（[keep] 为真）
 * 对 Chromium 仍报告「可见」，网页接着播放、接着被处理。没在出声的标签页照常暂停，不耗电。
 */
class AsmrWebView(context: Context) : WebView(context) {
    var keep: () -> Boolean = { false }

    override fun onWindowVisibilityChanged(visibility: Int) {
        super.onWindowVisibilityChanged(if (visibility != View.VISIBLE && keep()) View.VISIBLE else visibility)
    }
}

/** 一个标签页：一个 WebView。 */
class Tab(val id: Int, val web: AsmrWebView) {
    var title by mutableStateOf("")
    var url by mutableStateOf("")
    var progress by mutableStateOf(100)
    var canBack by mutableStateOf(false)
    var canForward by mutableStateOf(false)
    /** 网页里有没有正在出声的媒体 / Web Audio（注入的脚本报告；各个框架分开记）。 */
    var audible by mutableStateOf(false)
    /** 桌面版网站（换成电脑 Chrome 的 UA）。 */
    var desktop by mutableStateOf(false)
    val frames = HashMap<String, Boolean>()
    /** 每个框架（含 iframe）的消息通道：暂停 / 继续时给每个框架发消息。 */
    val proxies = HashMap<String, JavaScriptReplyProxy>()
}

/**
 * 浏览器：标签页、地址栏、发声检测、只放一个标签页的声音。
 *
 * - 发声检测：每个网页（包括 iframe）在文档开始时注入 [AUDIO_JS]，媒体播放 / 暂停、Web Audio 开关时报告给 App。
 * - 只放一个标签页的声音（默认开）：其它标签页用 `WebViewCompat.setAudioMuted` 静音——静音的标签页根本不出声，
 *   引擎只处理留下的那一个。默认留当前标签页；可以把某个标签页「固定」下来，切到别的标签页浏览时它继续出声。
 * - 网站对 WebView 的特殊对待：UA 去掉 `; wv`；拦掉 `bilibili://` 这类唤起 App 的跳转；B 站默认用桌面版。
 */
class Browser(private val context: Context) {
    val tabs = mutableStateListOf<Tab>()
    var current by mutableStateOf<Tab?>(null)
        private set
    /** 固定出声的标签页；null = 跟随当前标签页。 */
    var pinned by mutableStateOf<Tab?>(null)
        private set
    /** 只放一个标签页的声音。 */
    var onlyOne by mutableStateOf(true)
        private set
    /** 网页请求全屏（视频全屏）时的视图。 */
    var fullscreen by mutableStateOf<View?>(null)
        private set
    private var fullscreenDone: WebChromeClient.CustomViewCallback? = null
    private var nextId = 1

    private val assets = WebViewAssetLoader.Builder()
        .addPathHandler("/assets/", WebViewAssetLoader.AssetsPathHandler(context))
        .build()
    private val mobileUa: String = WebSettings.getDefaultUserAgent(context).replace("; wv)", ")")

    /** 正在出声（会被引擎处理）的那个标签页。 */
    val audibleTab: Tab?
        get() = if (!onlyOne) tabs.firstOrNull { it.audible } else (pinned ?: current)?.takeIf { it.audible }

    fun newTab(url: String = HOME): Tab {
        val tab = Tab(nextId++, createWebView())
        tab.web.keep = { tab.audible && (!onlyOne || tab == (pinned ?: current)) }
        setupClients(tab)
        tabs += tab
        select(tab)
        open(tab, url)
        return tab
    }

    fun select(tab: Tab) {
        current = tab
        applyMute()
    }

    fun close(tab: Tab) {
        val i = tabs.indexOf(tab)
        if (i < 0) return
        tabs.removeAt(i)
        if (pinned == tab) pinned = null
        tab.web.stopLoading()
        tab.web.destroy()
        if (tabs.isEmpty()) newTab() else if (current == tab) select(tabs[(i - 1).coerceAtLeast(0)]) else applyMute()
    }

    /** 从通知栏退出时：停掉、销毁所有标签页（之后这个 Browser 不再用）。 */
    fun destroyAll() {
        for (t in tabs) {
            t.web.stopLoading()
            t.web.destroy()
        }
        tabs.clear()
        pinned = null
        current = null
    }

    fun togglePin(tab: Tab) {
        pinned = if (pinned == tab) null else tab
        applyMute()
    }

    fun changeOnlyOne(on: Boolean) {
        onlyOne = on
        applyMute()
    }

    fun setDesktop(tab: Tab, on: Boolean) {
        if (tab.desktop == on) return
        tab.desktop = on
        tab.web.settings.userAgentString = if (on) DESKTOP_UA else mobileUa
        tab.web.reload()
    }

    /** 地址栏输入：网址直接打开，其余当作搜索。 */
    fun open(tab: Tab, input: String) {
        val s = input.trim()
        val url = when {
            s.isEmpty() -> return
            s.contains("://") -> s
            !s.contains(' ') && s.contains('.') -> "https://$s"
            else -> "https://www.bing.com/search?q=" + Uri.encode(s)
        }
        val host = Uri.parse(url).host.orEmpty()
        if (DESKTOP_HOSTS.any { host == it || host.endsWith(".$it") } && !tab.desktop) {
            tab.desktop = true
            tab.web.settings.userAgentString = DESKTOP_UA
        }
        Log.i(TAG, "打开 $url")
        tab.web.loadUrl(url)
    }

    /** 让不该出声的标签页静音。 */
    fun applyMute() {
        if (!WebViewFeature.isFeatureSupported(WebViewFeature.MUTE_AUDIO)) return
        val keep = pinned ?: current
        for (t in tabs) WebViewCompat.setAudioMuted(t.web, onlyOne && t != keep)
    }

    /**
     * 让这个标签页重新开一条音频输出流：先静音约 6 秒（Chromium 约 5 秒后关掉旧流），再恢复。
     * 截取自检换成备用 hook 后用：备用 hook 只截得到之后新开的流。
     */
    fun reopenAudio(tab: Tab) {
        if (!WebViewFeature.isFeatureSupported(WebViewFeature.MUTE_AUDIO)) return
        WebViewCompat.setAudioMuted(tab.web, true)
        tab.web.postDelayed({ applyMute() }, 6500)
    }

    /** 给正在出声的标签页（只放一个时是固定的或当前的，否则所有标签页）的每个框架发消息：pause / play。 */
    fun sendToAudible(msg: String) {
        val targets = if (onlyOne) listOfNotNull(pinned ?: current) else tabs.toList()
        for (t in targets) for (p in t.proxies.values) runCatching { p.postMessage(msg) }
    }

    fun exitFullscreen() {
        fullscreenDone?.onCustomViewHidden()
        fullscreenDone = null
        fullscreen = null
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun createWebView(): AsmrWebView {
        val web = AsmrWebView(context)
        web.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            mediaPlaybackRequiresUserGesture = false
            loadWithOverviewMode = true
            useWideViewPort = true
            builtInZoomControls = true
            displayZoomControls = false
            userAgentString = mobileUa
        }
        CookieManager.getInstance().setAcceptThirdPartyCookies(web, true)
        return web
    }

    private fun setupClients(tab: Tab) {
        val web = tab.web
        if (WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) {
            WebViewCompat.addDocumentStartJavaScript(web, AUDIO_JS, setOf("*"))
        }
        if (WebViewFeature.isFeatureSupported(WebViewFeature.WEB_MESSAGE_LISTENER)) {
            WebViewCompat.addWebMessageListener(web, "asmrAudio", setOf("*")) { _, msg, _, _, reply ->
                val o = runCatching { JSONObject(msg.data ?: "") }.getOrNull() ?: return@addWebMessageListener
                val id = o.optString("id")
                tab.frames[id] = o.optBoolean("on")
                tab.proxies[id] = reply
                val on = tab.frames.values.any { it }
                if (on != tab.audible) {
                    tab.audible = on
                    Log.i(TAG, "标签页 ${tab.id} ${if (on) "开始" else "停止"}出声：${tab.title}")
                }
            }
        }
        web.webViewClient = object : WebViewClient() {
            override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? =
                assets.shouldInterceptRequest(request.url)

            // 网站常把移动版页面重定向到 bilibili:// 之类唤起 App 的链接，WebView 打不开，直接拦掉留在网页里
            // （只有 App 自带首页上用户点的 B 站主页链接例外，见下面）
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                val scheme = request.url.scheme?.lowercase()
                if (scheme == "http" || scheme == "https") {
                    val host = request.url.host.orEmpty()
                    if (request.isForMainFrame && !tab.desktop && DESKTOP_HOSTS.any { host == it || host.endsWith(".$it") }) {
                        open(tab, request.url.toString())
                        return true
                    }
                    return false
                }
                // App 自带首页上的「随意Official」：用户点的才唤起 B 站 App，没装时在这个标签页里打开网页版主页
                if (request.isForMainFrame && request.hasGesture() && view.url?.startsWith(HOME) == true &&
                    request.url.toString() == AppLinks.BILIBILI_APP_URI
                ) {
                    AppLinks.openBilibili(context) { open(tab, AppLinks.BILIBILI_URL) }
                    return true
                }
                Log.i(TAG, "拦掉了非网页链接：${request.url}")
                return true
            }

            override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) {
                tab.url = url
                tab.frames.clear()
                tab.proxies.clear()
                tab.audible = false
                update(tab)
            }

            override fun onPageFinished(view: WebView, url: String) {
                tab.url = url
                update(tab)
            }

            override fun doUpdateVisitedHistory(view: WebView, url: String, isReload: Boolean) {
                tab.url = url
                update(tab)
            }
        }
        web.webChromeClient = object : WebChromeClient() {
            override fun onProgressChanged(view: WebView, newProgress: Int) {
                tab.progress = newProgress
            }

            override fun onReceivedTitle(view: WebView, title: String) {
                tab.title = title
            }

            override fun onConsoleMessage(m: ConsoleMessage): Boolean {
                Log.i(PAGE_TAG, m.message())
                return true
            }

            override fun onShowCustomView(view: View, callback: CustomViewCallback) {
                fullscreenDone?.onCustomViewHidden()
                fullscreen = view
                fullscreenDone = callback
            }

            override fun onHideCustomView() {
                fullscreen = null
                fullscreenDone = null
            }
        }
    }

    private fun update(tab: Tab) {
        tab.canBack = tab.web.canGoBack()
        tab.canForward = tab.web.canGoForward()
    }

    companion object {
        const val TAG = "AsmrBrowser"
        const val PAGE_TAG = "AsmrPage"
        const val HOME = "https://appassets.androidplatform.net/assets/home.html"
        const val TEST_URL = "https://appassets.androidplatform.net/assets/test.html"
        const val DESKTOP_UA =
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36"
        /** 移动版只引导装 App、不在网页里播放的网站：默认用桌面版。 */
        val DESKTOP_HOSTS = setOf("bilibili.com")

        /**
         * 发声检测：媒体元素（包括没放进页面的 `new Audio()`）在播放、没静音、音量不为 0，或者有 AudioContext 在运行，
         * 就算出声。状态变了才报告；每个框架一个随机 id，App 按框架分别记。
         */
        val AUDIO_JS = """
(() => {
  if (window.__asmrAudio) return;
  window.__asmrAudio = true;
  const id = Math.random().toString(36).slice(2);
  const media = new Set(), ctxs = new Set();
  let last = null;
  const send = (on) => { try { asmrAudio.postMessage(JSON.stringify({ id, on })); } catch (e) {} };
  const report = () => {
    let on = false;
    const all = new Set([...media, ...document.querySelectorAll('video,audio')]);
    for (const m of all) if (!m.paused && !m.ended && !m.muted && m.volume > 0) { on = true; break; }
    if (!on) for (const c of ctxs) if (c.state === 'running') { on = true; break; }
    if (on !== last) { last = on; send(on); }
  };
  const later = () => setTimeout(report, 0);
  const EV = ['play', 'playing', 'pause', 'ended', 'volumechange', 'emptied'];
  const track = (m) => { if (media.has(m)) return; media.add(m); for (const e of EV) m.addEventListener(e, later); };
  const play = HTMLMediaElement.prototype.play;
  HTMLMediaElement.prototype.play = function (...a) { track(this); later(); return play.apply(this, a); };
  for (const e of EV) document.addEventListener(e, (ev) => { if (ev.target instanceof HTMLMediaElement) track(ev.target); later(); }, true);
  for (const name of ['AudioContext', 'webkitAudioContext']) {
    const AC = window[name];
    if (!AC) continue;
    const W = function (...a) { const c = new AC(...a); ctxs.add(c); c.addEventListener('statechange', later); later(); return c; };
    W.prototype = AC.prototype;
    window[name] = W;
  }
  // App 发来的暂停 / 继续（通知栏、耳机按键）：只继续自己暂停的那些
  const held = new Set(), heldCtx = new Set();
  const onMsg = (ev) => {
    if (ev.data === 'pause') {
      for (const m of new Set([...media, ...document.querySelectorAll('video,audio')])) if (!m.paused) { held.add(m); m.pause(); }
      for (const c of ctxs) if (c.state === 'running') { heldCtx.add(c); c.suspend(); }
    } else if (ev.data === 'play') {
      for (const m of held) m.play().catch(() => {});
      for (const c of heldCtx) c.resume();
      held.clear(); heldCtx.clear();
    }
    later();
  };
  try { asmrAudio.addEventListener('message', onMsg); } catch (e) {}
  setInterval(report, 2000);
  send(false);
  addEventListener('pagehide', () => { last = false; send(false); });
})();
"""
    }
}
