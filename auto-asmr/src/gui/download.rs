//! 按链接下载网络音频（主要是 B 站）：调用 yt-dlp 只下音频，存进运行目录下的 `bilibili` 文件夹，
//! 下好后由界面加进文件页的列表，之后和本地文件一样试听、合成、自动对齐。
//!
//! yt-dlp 不随程序分发。依次在运行目录、程序所在目录、PATH 里找（macOS / Linux 再看几个常见的安装位置：
//! 从访达、桌面启动的程序拿不到终端里的 PATH）。找不到时界面给一个按钮，从 yt-dlp 在 GitHub 的官方发布页
//! 下载本平台的单文件版放进运行目录，按同一发布里的 SHA2-256SUMS 校验、试运行一次后再用。
//! 下载用系统自带的 curl（Windows 10 1803 起、macOS、常见的 Linux 发行版都有），程序本身不带 HTTPS 库。

use super::t;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 下载的音频放在运行目录下的这个文件夹里。
pub const DOWNLOAD_DIR: &str = "bilibili";

/// yt-dlp 官方发布页（最新版）的下载地址前缀。
const RELEASE_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";

/// yt-dlp 的安装说明（自动下载失败时给出这个，让用户自己装）。
pub const YTDLP_HOME: &str = "https://github.com/yt-dlp/yt-dlp#installation";

/// 下载音频时给 yt-dlp 的参数（目录和链接另外加）。
/// - 只下音频，优先 AAC（`mp4a`，程序能直接解码）；没有单独的音频流时退回整个视频。
/// - 不用 ffmpeg 修正容器（`--fixup never`）：B 站的 DASH 音频本来就能直接解码，这样有没有装 ffmpeg 结果都一样。
/// - 同一个地址只重试 2 次：B 站的 CDN 拒绝一个签名地址（HTTP 567）后，这个地址再试多少次都不行，
///   要从头重新解析拿新地址（见 [`fetch_audio`]）。
/// - 链接里是分 P 视频的某一 P 时只下这一 P（`--no-playlist`）；不读用户自己的 yt-dlp 配置，免得输出位置、格式被改掉。
/// - 标准输出只有三种行：`@@T 标题`（开始下载前）、`@@P 已下字节 总字节 估计总字节 速度`（进度）、
///   `@@F 文件路径`（下好、改好名之后；文件已经下过时也会有）。
const YTDLP_ARGS: &[&str] = &[
    "--ignore-config",
    "--no-playlist",
    "-f",
    "ba[acodec^=mp4a]/ba/b",
    "--fixup",
    "never",
    "--no-mtime",
    "--encoding",
    "utf-8",
    "--socket-timeout",
    "20",
    "--retries",
    "2",
    "-o",
    "%(title).80B [%(id)s].%(ext)s",
    "--print",
    "before_dl:@@T %(title)s",
    "--print",
    "after_move:@@F %(filepath)s",
    "--no-simulate",
    "--progress",
    "--newline",
    "--progress-template",
    "download:@@P %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s",
];

pub fn exe_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

/// 运行目录（配置文件 auto-asmr.ron 也在这里）。
pub fn run_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 下载的音频放在哪：运行目录下的 `bilibili`。
pub fn download_dir() -> PathBuf {
    run_dir().join(DOWNLOAD_DIR)
}

/// 找本机的 yt-dlp：运行目录（自动下载的放在这里）→ 程序所在目录 → PATH →（macOS / Linux）常见安装位置。
pub fn find_ytdlp() -> Option<PathBuf> {
    let mut dirs = vec![run_dir()];
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(d);
    }
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    #[cfg(not(windows))]
    {
        if let Some(h) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(h).join(".local/bin"));
        }
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    find_in(&dirs)
}

fn find_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(exe_name())).find(|p| p.is_file())
}

/// 本平台在 yt-dlp 发布页上的单文件版文件名；没有现成的（比如 32 位 ARM）时为 None。
pub fn release_asset() -> Option<&'static str> {
    let arch = std::env::consts::ARCH;
    if cfg!(windows) {
        match arch {
            "x86_64" => Some("yt-dlp.exe"),
            "aarch64" => Some("yt-dlp_arm64.exe"),
            "x86" => Some("yt-dlp_x86.exe"),
            _ => None,
        }
    } else if cfg!(target_os = "macos") {
        Some("yt-dlp_macos")
    } else if cfg!(target_os = "linux") {
        match (arch, cfg!(target_env = "musl")) {
            ("x86_64", false) => Some("yt-dlp_linux"),
            ("aarch64", false) => Some("yt-dlp_linux_aarch64"),
            ("x86_64", true) => Some("yt-dlp_musllinux"),
            ("aarch64", true) => Some("yt-dlp_musllinux_aarch64"),
            _ => None,
        }
    } else {
        None
    }
}

/// 自动下载的 yt-dlp 大约多大（MB，写在按钮的说明里）。
pub fn release_size_mb() -> u32 {
    if cfg!(windows) {
        24
    } else if cfg!(target_os = "macos") {
        38
    } else {
        41
    }
}

/// 从粘贴的文字里找出要下载的链接，按出现的顺序、去掉重复：
/// 完整的网址（B 站 App 的分享文字「【标题】 https://b23.tv/xxxx」也行）、没写 https:// 的 B 站网址、
/// 单独的 BV 号 / av 号。
pub fn extract_links(text: &str) -> Vec<String> {
    const EDGE: &[char] = &['"', '\'', '(', ')', '[', ']', '{', '}', '<', '>', ',', '.', ';', ':', '!', '?'];
    let mut out: Vec<String> = Vec::new();
    // 网址都是 ASCII：遇到空白或中文（包括中文标点）就断开
    for token in text.split(|c: char| c.is_whitespace() || !c.is_ascii()) {
        let link = if let Some(i) = token.find("https://").or_else(|| token.find("http://")) {
            let url = token[i..].trim_end_matches(EDGE);
            (url.len() > url.find("://").unwrap_or(0) + 3).then(|| url.to_string())
        } else {
            let w = token.trim_matches(EDGE);
            let lower = w.to_ascii_lowercase();
            if ["bilibili.com/", "b23.tv/", "bili2233.cn/"].iter().any(|d| lower.starts_with(d) || lower.contains(&format!(".{d}"))) {
                Some(format!("https://{w}"))
            } else if w.len() == 12 && lower.starts_with("bv") && w.bytes().all(|b| b.is_ascii_alphanumeric()) {
                Some(format!("https://www.bilibili.com/video/BV{}", &w[2..]))
            } else if w.len() > 2 && lower.starts_with("av") && w[2..].bytes().all(|b| b.is_ascii_digit()) {
                Some(format!("https://www.bilibili.com/video/av{}", &w[2..]))
            } else {
                None
            }
        };
        if let Some(l) = link {
            if !out.contains(&l) {
                out.push(l);
            }
        }
    }
    out
}

/// 下载进度。
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Progress {
    /// 完成的比例；不知道总大小时为 None。
    pub frac: Option<f32>,
    /// 已下载的字节数。
    pub bytes: u64,
    /// 速度（字节 / 秒）。
    pub speed: Option<f64>,
}

/// 后台任务发给界面的消息。
#[derive(Debug, PartialEq)]
pub enum Msg {
    /// 正在下载的视频标题。
    Title(String),
    Progress(Progress),
    /// 下好了一个文件（完整路径）。
    File(PathBuf),
    /// 上一次没下成，开始第 n 次尝试（从 2 开始）。
    Attempt(u32),
    /// 任务结束：Ok(说明) / Err(出错的原因)。最后一条消息。
    Done(Result<String, String>),
}

/// 一个后台任务（下载音频、下载或更新 yt-dlp）。
pub struct Task {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    finished: bool,
}

impl Task {
    fn spawn(f: impl FnOnce(&Sender<Msg>, &AtomicBool) -> Result<String, String> + Send + 'static) -> Task {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        std::thread::spawn(move || {
            let r = f(&tx, &c);
            let _ = tx.send(Msg::Done(r));
        });
        Task { rx, cancel, finished: false }
    }

    /// 取出到现在为止的消息。工作线程意外退出（没发 Done）时补一条 Done(Err)。
    pub fn poll(&mut self) -> Vec<Msg> {
        let mut v = Vec::new();
        while !self.finished {
            match self.rx.try_recv() {
                Ok(m) => {
                    self.finished = matches!(m, Msg::Done(_));
                    v.push(m);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finished = true;
                    v.push(Msg::Done(Err(t("下载线程意外退出", "The download thread stopped unexpectedly").to_string())));
                }
            }
        }
        v
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 取消并等它收尾（结束 yt-dlp / curl 进程），最多等 `max`。关窗口时用，免得留下还在下载的进程。
    pub fn cancel_and_wait(&mut self, max: Duration) {
        self.cancel();
        let t0 = Instant::now();
        while !self.finished && t0.elapsed() < max {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Msg::Done(_)) | Err(RecvTimeoutError::Disconnected) => self.finished = true,
                _ => {}
            }
        }
    }
}

/// 一个链接最多从头尝试几次。
const ATTEMPTS: u32 = 5;

/// 用 yt-dlp 下载一个链接的音频到 [`download_dir`]。
pub fn fetch_audio(ytdlp: PathBuf, url: String) -> Task {
    Task::spawn(move |tx, cancel| {
        let dir = download_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{} {}：{e}", t("无法新建文件夹", "Cannot create folder"), dir.display()))?;
        let mut files = 0;
        let mut err = String::new();
        // B 站的 CDN 有时拒绝分到的地址（HTTP 567，海外网络、开着代理时常见），yt-dlp 对同一个地址重试没用；
        // 从头再解析一次会拿到新地址，常常就能下了，所以失败了整体再来（实测海外网络下每次约 1/5–1/2 能成）。
        // 没下完的 .part 文件 yt-dlp 会接着下
        for attempt in 1..=ATTEMPTS {
            if attempt > 1 {
                let _ = tx.send(Msg::Attempt(attempt));
            }
            let mut cmd = Command::new(&ytdlp);
            cmd.args(YTDLP_ARGS).arg("-P").arg(&dir).arg("--").arg(&url);
            python_env(&mut cmd);
            let mut errs = Vec::new();
            let status = run(cmd, cancel, |l| match l {
                Line::Out(s) => {
                    if let Some(m) = parse_ytdlp_line(&s) {
                        if matches!(m, Msg::File(_)) {
                            files += 1;
                        }
                        let _ = tx.send(m);
                    }
                }
                Line::Err(s) => errs.push(s),
            })
            .map_err(|e| format!("{}（{}）：{e}", t("无法运行 yt-dlp", "Cannot run yt-dlp"), ytdlp.display()))?;
            if cancel.load(Ordering::Relaxed) {
                return Err(t("已取消", "Cancelled").to_string());
            }
            if status.success() && files > 0 {
                return Ok(String::new());
            }
            err = if status.success() { t("没有下载到音频", "No audio was downloaded").to_string() } else { explain(&error_text(&errs, status)) };
            // 有的已经下好了（比如合集里的几个）、正常结束却什么都没下（空的合集），或者重试也没用的错误
            // （链接不对、视频没了），不再从头来
            if files > 0 || status.success() || !worth_retrying(&err) {
                break;
            }
        }
        Err(err)
    })
}

/// 从 yt-dlp 官方发布页下载本平台的单文件版，放进运行目录。成功时说明里是它的版本号。
pub fn install_ytdlp() -> Task {
    Task::spawn(|tx, cancel| {
        let asset = release_asset().ok_or_else(|| format!("{}：{YTDLP_HOME}", t("这个系统没有现成的 yt-dlp 单文件版，请自己安装", "No ready-made yt-dlp for this system; please install it yourself")))?;
        let dir = run_dir();
        let dest = dir.join(exe_name());
        let tmp = dir.join(format!("{}.download", exe_name()));
        let fail = |e: String| {
            let _ = std::fs::remove_file(&tmp);
            e
        };
        // 1. 下载
        let mut cmd = Command::new("curl");
        cmd.args(["-fL", "--retry", "2", "--connect-timeout", "20", "-#", "-o"]).arg(&tmp).arg(format!("{RELEASE_URL}/{asset}"));
        let mut errs = Vec::new();
        let status = run(cmd, cancel, |l| {
            if let Line::Err(s) = l {
                match curl_percent(&s) {
                    Some(f) => {
                        let bytes = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
                        let _ = tx.send(Msg::Progress(Progress { frac: Some(f), bytes, speed: None }));
                    }
                    None => errs.push(s),
                }
            }
        })
        .map_err(|e| fail(no_curl(&e, &dir)))?;
        if cancel.load(Ordering::Relaxed) {
            return Err(fail(t("已取消", "Cancelled").to_string()));
        }
        if !status.success() {
            return Err(fail(format!("{}：{}", t("下载 yt-dlp 失败", "Downloading yt-dlp failed"), error_text(&errs, status))));
        }
        // 2. 按同一发布里的 SHA2-256SUMS 校验
        let sums = curl_text(&format!("{RELEASE_URL}/SHA2-256SUMS"), cancel).map_err(|e| fail(format!("{}：{e}", t("下载校验文件失败", "Downloading the checksums failed"))))?;
        let want = checksum_for(&sums, asset).ok_or_else(|| fail(t("校验文件里没有这个文件", "The checksum file does not list this file").to_string()))?;
        let got = std::fs::File::open(&tmp).and_then(sha256_hex).map_err(|e| fail(e.to_string()))?;
        if got != want {
            return Err(fail(t("下载的 yt-dlp 校验不对（可能没下完整），请重试", "The downloaded yt-dlp failed the checksum (incomplete download?); please retry").to_string()));
        }
        // 3. 设成可执行，放到位，试运行一次
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| fail(e.to_string()))?;
        }
        std::fs::rename(&tmp, &dest).map_err(|e| fail(format!("{} {}：{e}", t("无法写入", "Cannot write"), dest.display())))?;
        ytdlp_version(&dest, cancel).map_err(|e| {
            let _ = std::fs::remove_file(&dest);
            format!("{}：{e}", t("下载的 yt-dlp 运行不起来", "The downloaded yt-dlp does not run"))
        })
    })
}

/// 让 yt-dlp 自己更新到最新版（`yt-dlp -U`）。B 站改了接口、下载失败时用。
/// 用包管理器（brew、pip、winget……）装的 yt-dlp 不能自己更新，yt-dlp 会说该怎么更新，原样显示。
pub fn update_ytdlp(ytdlp: PathBuf) -> Task {
    Task::spawn(move |_tx, cancel| {
        let mut cmd = Command::new(&ytdlp);
        cmd.arg("-U");
        python_env(&mut cmd);
        let mut lines = Vec::new();
        let status = run(cmd, cancel, |l| match l {
            Line::Out(s) | Line::Err(s) => lines.push(s),
        })
        .map_err(|e| format!("{}：{e}", t("无法运行 yt-dlp", "Cannot run yt-dlp")))?;
        if cancel.load(Ordering::Relaxed) {
            return Err(t("已取消", "Cancelled").to_string());
        }
        let last = lines.iter().rev().find(|s| !s.trim().is_empty()).cloned().unwrap_or_default();
        if status.success() {
            Ok(last)
        } else {
            Err(error_text(&lines, status))
        }
    })
}

/// 用系统的文件管理器打开一个文件夹（没有就先建）。
pub fn open_folder(p: &Path) {
    let _ = std::fs::create_dir_all(p);
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener).arg(p).spawn();
}

// ---------------------------------------------------------------- 运行外部程序

enum Line {
    Out(String),
    Err(String),
}

/// yt-dlp 是 Python 程序：输出统一用 UTF-8（Windows 上默认是本地代码页），不带颜色。
fn python_env(cmd: &mut Command) {
    cmd.env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1").env("NO_COLOR", "1");
}

/// Windows 上图形程序启动命令行程序时不弹出黑色的控制台窗口。
fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// 运行外部程序，标准输出、标准错误按行（`\n` 或 `\r` 断开，进度条用 `\r` 刷新）交给 `on_line`。
/// `cancel` 置位后先让它自己退出，3 秒还没退出就强制结束。
fn run(mut cmd: Command, cancel: &AtomicBool, mut on_line: impl FnMut(Line)) -> std::io::Result<ExitStatus> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    no_window(&mut cmd);
    let mut child = cmd.spawn()?;
    let (tx, rx) = channel();
    let out = child.stdout.take();
    let err = child.stderr.take();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        if let Some(r) = out {
            read_lines(r, |s| {
                let _ = tx.send(Line::Out(s));
            });
        }
    });
    std::thread::spawn(move || {
        if let Some(r) = err {
            read_lines(r, |s| {
                let _ = tx2.send(Line::Err(s));
            });
        }
    });
    let mut stopping: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(l) => on_line(l),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break, // 两边的输出都关了：程序已经退出
        }
        if cancel.load(Ordering::Relaxed) {
            match stopping {
                None => {
                    terminate(&child);
                    stopping = Some(Instant::now());
                }
                Some(t0) if t0.elapsed() > Duration::from_secs(3) => {
                    let _ = child.kill();
                    break;
                }
                _ => {}
            }
        }
    }
    child.wait()
}

/// 让进程（连同它的子进程）退出。yt-dlp 的单文件版是"引导程序 + 真正的 Python 进程"两层：
/// Windows 上用 taskkill /T 结束整棵进程树；macOS / Linux 上发 SIGTERM，引导程序会转给子进程。
fn terminate(child: &Child) {
    let pid = child.id().to_string();
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("taskkill");
        c.args(["/T", "/F", "/PID", &pid]);
        c
    } else {
        let mut c = Command::new("kill");
        c.args(["-TERM", &pid]);
        c
    };
    no_window(&mut cmd);
    let _ = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

fn read_lines(mut r: impl Read, mut f: impl FnMut(String)) {
    let mut buf = [0u8; 4096];
    let mut cur = Vec::new();
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' || b == b'\r' {
                if !cur.is_empty() {
                    f(String::from_utf8_lossy(&cur).into_owned());
                    cur.clear();
                }
            } else {
                cur.push(b);
            }
        }
    }
    if !cur.is_empty() {
        f(String::from_utf8_lossy(&cur).into_owned());
    }
}

/// 出错时给用户看的一句话：yt-dlp / curl 最后一条 ERROR（没有就用最后一行），再不行就写退出码。
/// yt-dlp 的错误行有时是 "ERROR: \r[download] Got error: …"，按 `\r` 断开后原因在下一行。
fn error_text(lines: &[String], status: ExitStatus) -> String {
    if let Some(i) = lines.iter().rposition(|s| s.trim_start().starts_with("ERROR:")) {
        let rest = lines[i].trim_start()["ERROR:".len()..].trim();
        if !rest.is_empty() {
            return rest.to_string();
        }
        if let Some(next) = lines[i + 1..].iter().map(|s| s.trim()).find(|s| !s.is_empty()) {
            return next.to_string();
        }
    }
    match lines.iter().rev().map(|s| s.trim()).find(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => format!("{} {status}", t("退出码", "exit status")),
    }
}

/// 常见的下载错误前面加一句看得懂的说明（原文留在括号里，方便查）。
fn explain(e: &str) -> String {
    let lower = e.to_ascii_lowercase();
    let why = if lower.contains("http error 567") {
        t("B 站的视频服务器这次拒绝了下载（海外网络、开着代理时常见），过一会儿再试", "Bilibili's video server refused the download (common outside China or behind a proxy); try again later")
    } else if lower.contains("http error 412") {
        t("被 B 站的风控拦了，过一会儿再试，或者更新 yt-dlp", "Blocked by Bilibili's anti-crawler check; try again later or update yt-dlp")
    } else if lower.contains("geo-restricted") || lower.contains("may be deleted") || lower.contains("http error 404") {
        t("视频不存在、已删除，或者只在部分地区能看", "The video does not exist, was deleted, or is region-locked")
    } else if lower.contains("unsupported url") {
        t("不是视频链接（yt-dlp 不认识这个网址）", "Not a video link (yt-dlp does not support this URL)")
    } else if lower.contains("login") || lower.contains("premium") || lower.contains("vip") {
        t("要登录或大会员才能看", "Requires login or a premium membership")
    } else {
        return e.to_string();
    };
    format!("{why}（{e}）")
}

/// 这个错误从头再试一次有没有可能好：链接本身的问题（不认识、不存在、要登录）不重试。
fn worth_retrying(e: &str) -> bool {
    let lower = e.to_ascii_lowercase();
    !["unsupported url", "geo-restricted", "may be deleted", "http error 404", "login", "premium", "vip", "no video formats"].iter().any(|k| lower.contains(k))
}

fn no_curl(e: &std::io::Error, dir: &Path) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        format!(
            "{} {YTDLP_HOME} {} {}",
            t("没找到 curl，无法自动下载。请从", "curl was not found, so yt-dlp cannot be downloaded automatically. Download it from"),
            t("手动下载 yt-dlp，放进", "and put it in"),
            dir.display()
        )
    } else {
        format!("{}：{e}", t("无法运行 curl", "Cannot run curl"))
    }
}

/// curl `-#` 进度条的一行（"######    45.3%"）→ 完成比例。
fn curl_percent(s: &str) -> Option<f32> {
    let last = s.trim().rsplit(' ').next()?;
    let v: f32 = last.strip_suffix('%')?.parse().ok()?;
    Some((v / 100.0).clamp(0.0, 1.0))
}

/// 用 curl 取一个小的文本文件。
fn curl_text(url: &str, cancel: &AtomicBool) -> Result<String, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "--retry", "2", "--connect-timeout", "20", url]);
    let (mut out, mut errs) = (String::new(), Vec::new());
    let status = run(cmd, cancel, |l| match l {
        Line::Out(s) => {
            out.push_str(&s);
            out.push('\n');
        }
        Line::Err(s) => errs.push(s),
    })
    .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(out)
    } else {
        Err(error_text(&errs, status))
    }
}

/// `yt-dlp --version` 的输出（如 "2026.11.23"）。
fn ytdlp_version(p: &Path, cancel: &AtomicBool) -> Result<String, String> {
    let mut cmd = Command::new(p);
    cmd.arg("--version");
    python_env(&mut cmd);
    let (mut out, mut errs) = (Vec::new(), Vec::new());
    let status = run(cmd, cancel, |l| match l {
        Line::Out(s) => out.push(s),
        Line::Err(s) => errs.push(s),
    })
    .map_err(|e| e.to_string())?;
    match out.into_iter().map(|s| s.trim().to_string()).find(|s| !s.is_empty()) {
        Some(v) if status.success() => Ok(v),
        _ => Err(error_text(&errs, status)),
    }
}

/// yt-dlp 标准输出的一行（格式见 [`YTDLP_ARGS`]）→ 消息；别的行不要。
fn parse_ytdlp_line(s: &str) -> Option<Msg> {
    if let Some(r) = s.strip_prefix("@@T ") {
        Some(Msg::Title(r.trim().to_string()))
    } else if let Some(r) = s.strip_prefix("@@F ") {
        let r = r.trim();
        (!r.is_empty()).then(|| Msg::File(PathBuf::from(r)))
    } else if let Some(r) = s.strip_prefix("@@P ") {
        parse_progress(r).map(Msg::Progress)
    } else {
        None
    }
}

/// "已下字节 总字节 估计总字节 速度"，不知道的项是 NA。
fn parse_progress(r: &str) -> Option<Progress> {
    let f: Vec<Option<f64>> = r.split_whitespace().map(|x| x.parse::<f64>().ok().filter(|v| v.is_finite())).collect();
    if f.len() < 4 {
        return None;
    }
    let done = f[0];
    let total = f[1].or(f[2]).filter(|v| *v > 0.0);
    let frac = match (done, total) {
        (Some(d), Some(tot)) => Some((d / tot).clamp(0.0, 1.0) as f32),
        _ => None,
    };
    Some(Progress { frac, bytes: done.unwrap_or(0.0).max(0.0) as u64, speed: f[3] })
}

/// SHA2-256SUMS（每行"哈希  文件名"）里某个文件的哈希（小写十六进制）。
fn checksum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let hash = it.next()?;
        let file = it.next()?.trim_start_matches('*');
        (file == name && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

// ---------------------------------------------------------------- SHA-256（校验下载的 yt-dlp）

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    len: usize,
    total: u64,
}

impl Sha256 {
    fn new() -> Self {
        Sha256 { h: [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19], buf: [0; 64], len: 0, total: 0 }
    }

    fn update(&mut self, mut d: &[u8]) {
        self.total = self.total.wrapping_add(d.len() as u64);
        while !d.is_empty() {
            let n = (64 - self.len).min(d.len());
            self.buf[self.len..self.len + n].copy_from_slice(&d[..n]);
            self.len += n;
            d = &d[n..];
            if self.len == 64 {
                let b = self.buf;
                self.block(&b);
                self.len = 0;
            }
        }
    }

    fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.len != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, v) in self.h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    fn block(&mut self, b: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut bb, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & bb) ^ (a & c) ^ (bb & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = bb;
            bb = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in self.h.iter_mut().zip([a, bb, c, d, e, f, g, h]) {
            *x = x.wrapping_add(v);
        }
    }
}

/// 读完 `r`，返回 SHA-256（小写十六进制）。
fn sha256_hex(mut r: impl Read) -> std::io::Result<String> {
    let mut s = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        s.update(&buf[..n]);
    }
    Ok(s.finish().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_found_in_share_text() {
        // B 站 App 的分享文字：标题、中文标点和链接混在一起
        let v = extract_links("【【MV】保加利亚妖王AZIS视频合辑-哔哩哔哩】 https://b23.tv/AbCd123，快来看");
        assert_eq!(v, ["https://b23.tv/AbCd123"]);
        let v = extract_links("https://www.bilibili.com/video/BV17x411w7KC/?share_source=copy_web&p=2");
        assert_eq!(v, ["https://www.bilibili.com/video/BV17x411w7KC/?share_source=copy_web&p=2"]);
        // 单独的 BV 号 / av 号、没写 https:// 的网址、括号和句号
        let v = extract_links("BV17x411w7KC\nav170001\nwww.bilibili.com/video/BV1xx411c7mD?p=3\n(https://b23.tv/x9).\nm.bilibili.com/video/av2");
        assert_eq!(
            v,
            [
                "https://www.bilibili.com/video/BV17x411w7KC",
                "https://www.bilibili.com/video/av170001",
                "https://www.bilibili.com/video/BV1xx411c7mD?p=3",
                "https://b23.tv/x9",
                "https://m.bilibili.com/video/av2",
            ]
        );
        // 重复的只留一个；普通文字、半截网址不算
        assert_eq!(extract_links("BV17x411w7KC BV17x411w7KC https://www.bilibili.com/video/BV17x411w7KC https://www.bilibili.com/video/BV17x411w7KC?p=2").len(), 2);
        assert!(extract_links("hello world, average 12 https:// bv123").is_empty());
    }

    #[test]
    fn ytdlp_output_lines_are_parsed() {
        assert_eq!(parse_ytdlp_line("@@T 【MV】标题 p01"), Some(Msg::Title("【MV】标题 p01".into())));
        assert_eq!(parse_ytdlp_line("@@F /tmp/bilibili/标题 [BV17x411w7KC_p1].m4a"), Some(Msg::File(PathBuf::from("/tmp/bilibili/标题 [BV17x411w7KC_p1].m4a"))));
        // 已经下过的文件：已下字节是 NA
        assert_eq!(parse_ytdlp_line("@@P NA 1265842 NA NA"), Some(Msg::Progress(Progress { frac: None, bytes: 0, speed: None })));
        // 只知道估计的总大小
        let Some(Msg::Progress(p)) = parse_ytdlp_line("@@P 1024 NA 4096 2048.5") else { panic!() };
        assert_eq!((p.frac, p.bytes, p.speed), (Some(0.25), 1024, Some(2048.5)));
        assert_eq!(parse_ytdlp_line("[download] Destination: x.m4a"), None);
        assert_eq!(parse_ytdlp_line("@@F "), None);
        assert!((curl_percent("######################                     31.4%").unwrap() - 0.314).abs() < 1e-6);
        assert_eq!(curl_percent("curl: (22) The requested URL returned error: 404"), None);
    }

    #[test]
    fn error_text_prefers_the_last_error_line() {
        let st = std::process::Command::new(if cfg!(windows) { "cmd" } else { "false" }).args(if cfg!(windows) { &["/C", "exit 1"][..] } else { &[][..] }).status().unwrap();
        let lines = vec!["[BiliBili] x: Downloading webpage".to_string(), "ERROR: [BiliBili] x: This video may be deleted".to_string(), "WARNING: something".to_string()];
        assert_eq!(error_text(&lines, st), "[BiliBili] x: This video may be deleted");
        assert_eq!(error_text(&["only line".to_string()], st), "only line");
        // "ERROR: \r[download] Got error: …" 按 \r 断开后原因在下一行
        let lines = vec!["ERROR: ".to_string(), "[download] Got error: HTTP Error 567: Unknown Status. Giving up after 2 retries".to_string()];
        let e = error_text(&lines, st);
        assert_eq!(e, "[download] Got error: HTTP Error 567: Unknown Status. Giving up after 2 retries");
        assert!(explain(&e).ends_with(&format!("（{e}）")) && worth_retrying(&e));
        let gone = "[BiliBili] 1GJ411x7h7: This video may be deleted or geo-restricted. You might want to try a VPN";
        assert!(!worth_retrying(gone) && explain(gone).contains(gone));
        assert_eq!(explain("something odd"), "something odd");
    }

    #[test]
    fn checksums_and_sha256() {
        let sums = "aa  yt-dlp_x86.exe\nBA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD  yt-dlp_macos\n";
        assert_eq!(checksum_for(sums, "yt-dlp_macos").as_deref(), Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert_eq!(checksum_for(sums, "yt-dlp_x86.exe"), None); // 不是 64 位十六进制
        assert_eq!(checksum_for(sums, "yt-dlp"), None);
        // FIPS 180-2 的测试向量：空串、"abc"、两个分组的、一百万个 a（分块读）
        assert_eq!(sha256_hex(&b""[..]).unwrap(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(&b"abc"[..]).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_hex(&b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"[..]).unwrap(), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        assert_eq!(sha256_hex(&vec![b'a'; 1_000_000][..]).unwrap(), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }

    #[test]
    fn ytdlp_is_found_in_the_given_folders() {
        let base = std::env::temp_dir().join(format!("auto-asmr-ytdlp-test-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert_eq!(find_in(&[a.clone(), b.clone()]), None);
        std::fs::write(b.join(exe_name()), b"").unwrap();
        assert_eq!(find_in(&[a.clone(), b.clone()]), Some(b.join(exe_name())));
        // 排在前面的目录优先（运行目录里自动下载的那份先于 PATH 里的）
        std::fs::write(a.join(exe_name()), b"").unwrap();
        assert_eq!(find_in(&[a.clone(), b]), Some(a.join(exe_name())));
        let _ = std::fs::remove_dir_all(&base);
        assert!(release_asset().is_some(), "发布用的平台都该有现成的 yt-dlp");
    }

    #[test]
    fn cancelling_stops_the_process() {
        // 一个要跑很久的命令：取消后 run 在几秒内返回
        let cmd = if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        let cancel = AtomicBool::new(true);
        let t0 = Instant::now();
        let st = run(cmd, &cancel, |_| {}).unwrap();
        assert!(!st.success());
        assert!(t0.elapsed() < Duration::from_secs(5), "取消后用了 {:?}", t0.elapsed());
    }
}
