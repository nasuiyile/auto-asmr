package io.github.nasuiyile.autoasmr.files

import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.system.Os
import android.system.OsConstants
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.github.nasuiyile.autoasmr.NativeHook
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.util.concurrent.Executors

/**
 * 文件转换页在 Kotlin 这一侧：把命令交给 Rust（`rust/src/files.rs`，照搬桌面版文件页的逻辑），定时轮询整页状态；
 * 处理安卓特有的两件事：
 *
 * - **输入**：系统文件选择器给的是 content:// 地址。每个文件打开一个文件描述符，在缓存目录里建一个带原文件名的
 *   符号链接指向 `/proc/self/fd/N`，Rust 照样按路径读（不用复制）；读不了随机位置的（云盘之类）复制一份到缓存。
 *   「添加文件夹」记下文件夹的授权，输出默认放回这个文件夹（和电脑版「各自源文件旁边」一样）。
 * - **输出**：Rust 把结果写进 App 的暂存目录，完成后这里存到最终位置——「输出到…」选的文件、「输出文件夹…」选的
 *   文件夹、源文件所在的文件夹（添加文件夹时），否则「音乐/auto-asmr」（媒体库）；重名时加 `_2`、`_3`（同电脑版）。
 *   存好后告诉 Rust 最终位置（列表和说明里显示它），删掉暂存文件。
 */
object FileSession {
    private const val TAG = "AsmrFiles"

    /** Rust 给的整页状态。 */
    var st by mutableStateOf(JSONObject())
        private set
    /** 「输出文件夹…」选的文件夹（多个文件时用；空 = 默认）。 */
    var outTree by mutableStateOf<Uri?>(null)
        private set
    var outTreeName by mutableStateOf<String?>(null)
        private set
    /** 「输出到…」选的文件：单个文件时按输入的路径记，合成时记在 [MIX_KEY]。 */
    private val saveAs = HashMap<String, Uri>()
    var saveAsName by mutableStateOf<Map<String, String>>(emptyMap())
        private set
    var message by mutableStateOf<String?>(null)

    /** 界面在前台时轮询得勤（试听的位置图要动），否则只在转换、试听时慢慢轮询。 */
    @Volatile var uiVisible = false

    const val MIX_KEY = "#mix"

    private lateinit var app: Context
    private val main = Handler(Looper.getMainLooper())
    private val exec = Executors.newSingleThreadExecutor { Thread(it, "asmr-files") }
    private var polling = false

    /** 一个输入：原地址、给 Rust 的路径、保持打开的文件描述符、来自哪个文件夹（添加文件夹时）。 */
    private class Input(val uri: Uri, val path: String, val pfd: ParcelFileDescriptor?, val tree: Uri?, val name: String)
    private val inputs = LinkedHashMap<String, Input>() // 路径 → 输入
    private var seq = 0

    fun init(context: Context) {
        app = context.applicationContext
        val prefs = app.getSharedPreferences("ui", 0)
        prefs.getString("files_out_tree", null)?.let { outTree = Uri.parse(it) }
        outTreeName = prefs.getString("files_out_tree_name", null)
        // 上次的链接、没存完的暂存：进程重启后没用了
        File(app.cacheDir, "in").deleteRecursively()
        staging().deleteRecursively()
    }

    private fun staging() = File(app.filesDir, "out")

    // ---- 命令

    /** 发一条命令（在后台线程），状态更新到 [st]。 */
    fun cmd(op: String, vararg kv: Pair<String, Any?>) {
        val o = JSONObject().put("op", op)
        for ((k, v) in kv) o.put(k, v)
        exec.execute { send(o) }
    }

    private fun send(o: JSONObject) {
        val out = runCatching { NativeHook.files(o.toString().toByteArray(Charsets.UTF_8)) }.getOrNull() ?: return
        val s = runCatching { JSONObject(String(out, Charsets.UTF_8)) }.getOrNull() ?: return
        main.post { st = s }
        tidyInputs(s)
        deliver(s)
        ensurePolling(s)
    }

    private fun busy(s: JSONObject) = s.optBoolean("busy") || s.optBoolean("previewing") || s.optBoolean("decoding") || (s.optJSONArray("pending_out")?.length() ?: 0) > 0

    private fun ensurePolling(s: JSONObject) {
        if (polling || !(uiVisible || busy(s))) return
        polling = true
        main.postDelayed(pollTask, 200)
    }

    private val pollTask: Runnable = Runnable {
        exec.execute {
            send(JSONObject().put("op", "poll"))
            val s = st
            main.post {
                if (uiVisible || busy(s)) {
                    main.postDelayed(pollTask, if (uiVisible) 150 else 1000)
                } else {
                    polling = false
                }
            }
        }
    }

    /** 页面出现 / 消失。消失时停掉试听（不影响正在转换的任务）。 */
    fun setVisible(on: Boolean) {
        uiVisible = on
        if (on) {
            cmd("poll")
        } else {
            cmd("release")
        }
    }

    // ---- 输入

    private fun displayName(uri: Uri): String = runCatching {
        app.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c -> if (c.moveToFirst()) c.getString(0) else null }
    }.getOrNull() ?: uri.lastPathSegment?.substringAfterLast('/') ?: "audio"

    /** 「添加文件…」选的文件。 */
    fun addUris(uris: List<Uri>) {
        exec.execute {
            val paths = uris.mapNotNull { register(it, null, null) }
            send(JSONObject().put("op", "add").put("paths", JSONArray(paths)))
        }
    }

    /** 测试用（adb）：直接加入 App 自己能读的文件（外部文件目录里），不经过系统的文件选择器。 */
    fun addPaths(paths: List<String>) {
        exec.execute { send(JSONObject().put("op", "add").put("paths", JSONArray(paths))) }
    }

    /** 测试用（adb）：发一条任意命令（JSON）。`convert` 自动带上暂存目录。 */
    fun cmdJson(json: String) {
        val o = runCatching { JSONObject(json) }.getOrNull() ?: return
        if (o.optString("op") == "convert" && !o.has("staging")) o.put("staging", staging().path)
        exec.execute { send(o) }
    }

    /** 「添加文件夹…」：这一层的音频文件（不含子文件夹，跳过本程序的输出 _asmr），同电脑版。 */
    fun addTree(tree: Uri) {
        runCatching { app.contentResolver.takePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION) }
        exec.execute {
            val docId = DocumentsContract.getTreeDocumentId(tree)
            val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, docId)
            val found = ArrayList<Pair<Uri, String>>()
            runCatching {
                app.contentResolver.query(children, arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE), null, null, null)?.use { c ->
                    while (c.moveToNext()) {
                        val id = c.getString(0)
                        val name = c.getString(1) ?: continue
                        if (c.getString(2) == DocumentsContract.Document.MIME_TYPE_DIR) continue
                        if (isAudio(name) && !isOwnOutput(name)) found.add(DocumentsContract.buildDocumentUriUsingTree(tree, id) to name)
                    }
                }
            }
            found.sortBy { it.second }
            val paths = found.mapNotNull { (u, n) -> register(u, tree, n) }
            if (found.isEmpty()) main.post { message = "文件夹里没有找到新的音频文件" }
            send(JSONObject().put("op", "add").put("paths", JSONArray(paths)))
        }
    }

    /**
     * 给 Rust 一个路径（缓存目录里，带原文件名）。先试符号链接（指向打开的文件描述符，不用复制）：按这个路径重新打开
     * 要有读原文件的权限，系统文件选择器给的文件（分区存储）一般没有，读不了、或者不能随机读（云盘之类）时复制一份。
     * 已加过的返回原路径。
     */
    private fun register(uri: Uri, tree: Uri?, knownName: String?): String? {
        inputs.values.firstOrNull { it.uri == uri }?.let { return it.path }
        val name = (knownName ?: displayName(uri)).replace('/', '_')
        val dir = File(app.cacheDir, "in/${seq++}").apply { mkdirs() }
        val link = File(dir, name)
        return try {
            val pfd = app.contentResolver.openFileDescriptor(uri, "r") ?: return null
            val seekable = runCatching { Os.lseek(pfd.fileDescriptor, 0, OsConstants.SEEK_CUR); true }.getOrDefault(false)
            val linked = seekable && runCatching {
                Os.symlink("/proc/self/fd/${pfd.fd}", link.path)
                java.io.FileInputStream(link).close()
                true
            }.getOrElse {
                link.delete()
                false
            }
            if (linked) {
                inputs[link.path] = Input(uri, link.path, pfd, tree, name)
            } else {
                ParcelFileDescriptor.AutoCloseInputStream(pfd).use { i -> link.outputStream().use { o -> i.copyTo(o) } }
                inputs[link.path] = Input(uri, link.path, null, tree, name)
            }
            link.path
        } catch (e: Exception) {
            Log.w(TAG, "打不开 $uri", e)
            main.post { message = "读不了「$name」：${e.message}" }
            null
        }
    }

    /** Rust 的列表里没有了的输入（移除、清空）：关掉文件描述符、删掉链接。 */
    private fun tidyInputs(s: JSONObject) {
        val live = HashSet<String>()
        s.optJSONArray("files")?.let { a -> for (i in 0 until a.length()) live.add(a.getJSONObject(i).optString("path")) }
        val gone = inputs.keys.filter { it !in live }
        for (p in gone) {
            val inp = inputs.remove(p) ?: continue
            runCatching { inp.pfd?.close() }
            runCatching { File(p).delete(); File(p).parentFile?.delete() }
            saveAs.remove(p)
        }
        if (gone.isNotEmpty()) main.post { saveAsName = saveAsName.filterKeys { it in live || it == MIX_KEY } }
    }

    // ---- 输出位置

    fun chooseOutTree(tree: Uri?) {
        if (tree != null) runCatching { app.contentResolver.takePersistableUriPermission(tree, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION) }
        val name = tree?.let { treeName(it) }
        outTree = tree
        outTreeName = name
        app.getSharedPreferences("ui", 0).edit().putString("files_out_tree", tree?.toString()).putString("files_out_tree_name", name).apply()
    }

    /** 「输出到…」选的文件（`key` = 输入的路径，合成时是 [MIX_KEY]）；null = 恢复默认。 */
    fun setSaveAs(key: String, uri: Uri?) {
        if (uri == null) saveAs.remove(key) else saveAs[key] = uri
        val name = uri?.let { displayName(it) }
        saveAsName = if (name == null) saveAsName - key else saveAsName + (key to name)
    }

    private fun treeName(tree: Uri): String = runCatching {
        val doc = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree))
        app.contentResolver.query(doc, arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { c -> if (c.moveToFirst()) c.getString(0) else null }
    }.getOrNull() ?: "所选文件夹"

    /** 默认的输出位置怎么说（界面上显示）。 */
    fun defaultPlace(): String = "各自源文件所在的文件夹（添加文件夹时），否则「音乐/auto-asmr」"

    // ---- 存到最终位置

    private val delivering = HashSet<String>()

    private fun deliver(s: JSONObject) {
        val a = s.optJSONArray("pending_out") ?: return
        for (i in 0 until a.length()) {
            val o = a.getJSONObject(i)
            val out = o.optString("out")
            if (!delivering.add(out)) continue
            val input = o.optString("input")
            val mix = o.optBoolean("mix")
            val n = (s.optJSONArray("files")?.length() ?: 0)
            val label = try {
                saveOne(File(out), input, mix, n)
            } catch (e: Exception) {
                Log.w(TAG, "保存 $out 失败", e)
                "保存失败（${e.message}），文件还在 App 的暂存目录"
            }
            send(JSONObject().put("op", "delivered").put("out", out).put("label", label))
        }
    }

    /** 存一个输出，返回最终位置（显示用）。 */
    private fun saveOne(src: File, input: String, mix: Boolean, nFiles: Int): String {
        val name = src.name
        val key = if (mix) MIX_KEY else input
        val chosen = saveAs[key].takeIf { mix || nFiles == 1 }
        val label = when {
            chosen != null -> {
                app.contentResolver.openOutputStream(chosen, "wt")?.use { o -> src.inputStream().use { it.copyTo(o) } } ?: error("打不开所选文件")
                displayName(chosen)
            }
            outTree != null && !mix && nFiles > 1 -> saveToTree(src, outTree!!, outTreeName ?: "所选文件夹")
            inputs[input]?.tree != null -> saveToTree(src, inputs[input]!!.tree!!, treeName(inputs[input]!!.tree!!))
            else -> saveToMusic(src)
        }
        src.delete()
        src.parentFile?.delete() // 这一次的暂存子目录空了就删掉
        return label
    }

    /** 文件夹里还没有的名字：原名，重名时加 _2、_3（同电脑版）。 */
    private fun freeName(name: String, exists: (String) -> Boolean): String {
        if (!exists(name)) return name
        val stem = name.substringBeforeLast('.')
        val ext = name.substringAfterLast('.', "")
        var k = 2
        while (true) {
            val n = if (ext.isEmpty()) "${stem}_$k" else "${stem}_$k.$ext"
            if (!exists(n)) return n
            k++
        }
    }

    private fun saveToTree(src: File, tree: Uri, treeLabel: String): String {
        val parentId = DocumentsContract.getTreeDocumentId(tree)
        val parent = DocumentsContract.buildDocumentUriUsingTree(tree, parentId)
        val names = HashSet<String>()
        app.contentResolver.query(DocumentsContract.buildChildDocumentsUriUsingTree(tree, parentId), arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { c ->
            while (c.moveToNext()) c.getString(0)?.let { names.add(it) }
        }
        val name = freeName(src.name) { it in names }
        val doc = DocumentsContract.createDocument(app.contentResolver, parent, "audio/wav", name) ?: error("不能在「$treeLabel」里新建文件")
        app.contentResolver.openOutputStream(doc, "w")?.use { o -> src.inputStream().use { it.copyTo(o) } } ?: error("写不进「$treeLabel」")
        return "$treeLabel/$name"
    }

    private fun saveToMusic(src: File): String {
        val rel = "Music/auto-asmr/"
        val collection = MediaStore.Audio.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val name = freeName(src.name) { n ->
            app.contentResolver.query(collection, arrayOf(MediaStore.MediaColumns._ID), "${MediaStore.MediaColumns.RELATIVE_PATH}=? AND ${MediaStore.MediaColumns.DISPLAY_NAME}=?", arrayOf(rel, n), null)?.use { it.count > 0 } ?: false
        }
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
            put(MediaStore.MediaColumns.MIME_TYPE, "audio/wav")
            put(MediaStore.MediaColumns.RELATIVE_PATH, rel)
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val uri = app.contentResolver.insert(collection, values) ?: error("媒体库不让新建文件")
        try {
            app.contentResolver.openOutputStream(uri, "w")?.use { o -> src.inputStream().use { it.copyTo(o) } } ?: error("写不进媒体库")
            app.contentResolver.update(uri, ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }, null, null)
        } catch (e: Exception) {
            app.contentResolver.delete(uri, null, null)
            throw e
        }
        return "音乐/auto-asmr/$name"
    }

    /** 转换：输出先写进暂存目录，完成后存到最终位置（见类注释）。 */
    fun convert() = cmd("convert", "staging" to staging().path)

    // ---- 和桌面版相同的文件判断（batch.rs 的 AUDIO_EXTS、is_own_output）

    private val AUDIO_EXTS = setOf("wav", "m4a", "mp3", "flac", "ogg", "opus", "aac", "mp4", "mkv", "mov", "webm", "aiff", "aif", "caf")

    fun isAudio(name: String) = name.substringAfterLast('.', "").lowercase() in AUDIO_EXTS

    fun isOwnOutput(name: String): Boolean {
        val stem = name.substringBeforeLast('.')
        if (stem.endsWith("_asmr")) return true
        val k = stem.substringAfterLast("_asmr_", "")
        return stem.contains("_asmr_") && k.isNotEmpty() && k.all { it.isDigit() }
    }
}
