//! 一路声音的处理器（实时）：平时是一个引擎，立体声输入先合成单声道；「立体声宽度」大于 0、输入又有左右两个声道时，
//! 换成立体声对——左声道一个声源、右声道一个声源（见 [`pair_setup`]），相加之后统一收尾。
//!
//! 安卓版和浏览器插件处理网页声音、桌面版和安卓版试听单个文件都用它。配置随时可以换（[`Solo::set_config`]）；
//! 立体声宽度在 0 和大于 0 之间切换时，下一段就换到另一条处理路径，自动响度的增益接着用，听起来只是空间感变了。
//! 立体声对第一次用到时才建（两个引擎，会分配内存）；不想在音频线程里分配的调用方用 [`Solo::with_pair`] 事先建好。

use super::engine::{Engine, EngineConfig, PostMode, Telemetry};
use super::mix::{pair_setup, Mixer};
use std::sync::Arc;

/// 合成单声道时一次处理多少样本。
const CHUNK: usize = 1024;

pub struct Solo {
    one: Engine,
    pair: Option<Mixer>,
    post: PostMode,
    lookahead_s: f32,
    live: bool,
    mono: Vec<f32>,
    /// 上一段是不是按立体声对处理的。
    was_pair: bool,
}

impl Solo {
    /// `lookahead_s`：分析预读（实时为 0）。
    pub fn new(cfg: Arc<EngineConfig>, post: PostMode, lookahead_s: f32) -> Self {
        Solo { one: Engine::new(cfg, post, lookahead_s), pair: None, post, lookahead_s, live: false, mono: vec![0.0; CHUNK], was_pair: false }
    }

    /// 事先把立体声对建好：之后在 0 和大于 0 之间切换「立体声宽度」时不再分配内存。
    pub fn with_pair(mut self) -> Self {
        if self.pair.is_none() {
            self.pair = Some(self.make_pair());
        }
        self
    }

    fn make_pair(&self) -> Mixer {
        let mut m = Mixer::new(&pair_setup(self.one.config()), self.post, self.lookahead_s);
        m.set_live(self.live);
        m.set_live_balance(false);
        m
    }

    /// 主播模式的动作（停顿超过 1 秒后恢复正常速度），见 [`Engine::set_live`]。要在开始处理之前调用。
    pub fn set_live(&mut self, on: bool) {
        self.live = on;
        self.one.set_live(on);
        if let Some(m) = &mut self.pair {
            m.set_live(on);
            m.set_live_balance(false);
        }
    }

    /// 换配置（在控制块边界生效）。不分配内存。
    pub fn set_config(&mut self, cfg: Arc<EngineConfig>) {
        if let Some(m) = &mut self.pair {
            m.set_pair_config(&cfg);
        }
        self.one.set_config(cfg);
    }

    pub fn config(&self) -> &Arc<EngineConfig> {
        self.one.config()
    }

    /// 上一段是不是按立体声对处理的。
    pub fn is_pair(&self) -> bool {
        self.was_pair
    }

    /// 单个引擎（立体声对没在用时处理声音的那个）。
    pub fn engine(&self) -> &Engine {
        &self.one
    }

    /// 立体声对的两个引擎：[左声道, 右声道]（没在用立体声对时 None）。
    pub fn pair(&self) -> Option<[&Engine; 2]> {
        self.pair.as_ref().filter(|_| self.was_pair).map(|m| [m.engine(0), m.engine(1)])
    }

    /// 给界面显示的状态；立体声对时位置是两个声源的正中间。
    pub fn tele(&self) -> &Telemetry {
        match &self.pair {
            Some(m) if self.was_pair => &m.tele,
            _ => &self.one.tele,
        }
    }

    /// 算法延时（样本）：两条路径相同。
    pub fn latency_samples(&self) -> usize {
        match &self.pair {
            Some(m) if self.was_pair => m.latency_samples(),
            _ => self.one.latency_samples(),
        }
    }

    /// 处理一段立体声输入，输出等长的左右声道。
    pub fn process(&mut self, l: &[f32], r: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        let n = l.len().min(r.len()).min(out_l.len()).min(out_r.len());
        if self.one.config().params.stereo_width > 0.0 {
            if self.pair.is_none() {
                self.pair = Some(self.make_pair());
            }
            let m = self.pair.as_mut().unwrap();
            if !self.was_pair {
                // 从单个引擎切过来：自动响度的增益接着用（两条路径对同一段音乐的响度差是估计得出来的）
                m.set_leveler_gain_db(self.one.leveler_gain_db() + self.one.level_gain_db() - m.level_gain_db());
                self.was_pair = true;
            }
            m.process(&[&l[..n], &r[..n]], &mut out_l[..n], &mut out_r[..n]);
            return;
        }
        if self.was_pair {
            if let Some(m) = &self.pair {
                self.one.set_leveler_gain_db(m.leveler_gain_db() + m.level_gain_db() - self.one.level_gain_db());
            }
            self.was_pair = false;
        }
        let mut o = 0;
        while o < n {
            let k = (n - o).min(CHUNK);
            for (m, (a, b)) in self.mono[..k].iter_mut().zip(l[o..o + k].iter().zip(&r[o..o + k])) {
                *m = 0.5 * (a + b);
            }
            self.one.process(&self.mono[..k], &mut out_l[o..o + k], &mut out_r[o..o + k]);
            o += k;
        }
    }

    /// 处理一段单声道输入（麦克风、单声道的文件）：总是一个声源。
    pub fn process_mono(&mut self, x: &[f32], out_l: &mut [f32], out_r: &mut [f32]) {
        if self.was_pair {
            if let Some(m) = &self.pair {
                self.one.set_leveler_gain_db(m.leveler_gain_db() + m.level_gain_db() - self.one.level_gain_db());
            }
            self.was_pair = false;
        }
        self.one.process(x, out_l, out_r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::params::{MotionMode, Params};
    use crate::dsp::testsig::{lufs, test_song};
    use crate::dsp::FS_USIZE;

    fn energy(v: &[f32]) -> f64 {
        v.iter().map(|x| (x * x) as f64).sum::<f64>() + 1e-30
    }

    fn run(solo: &mut Solo, l: &[f32], r: &[f32], chunk: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut ol, mut or) = (vec![0f32; l.len()], vec![0f32; l.len()]);
        let mut o = 0;
        while o < l.len() {
            let e = (o + chunk).min(l.len());
            solo.process(&l[o..e], &r[o..e], &mut ol[o..e], &mut or[o..e]);
            o = e;
        }
        (ol, or)
    }

    /// 听歌：只在左声道里的声音从左前方来（左耳比右耳响几分贝，右耳也听得到）；把立体声宽度拖到 0，
    /// 下一段就合成一个声源（正前方，两耳一样响）；再拖回来又是两个声源。一次给多长输出都一样。
    #[test]
    fn stereo_width_turns_the_channels_into_two_sources_and_back() {
        let music = Params { auto_level: false, ..Params::default().style_music() };
        let cfg = Arc::new(EngineConfig::new(music.clone(), None));
        let song = test_song(6.0, 3);
        let silent = vec![0f32; song.len()];
        let mut solo = Solo::new(cfg.clone(), PostMode::Realtime, 0.0);
        let (l, r) = run(&mut solo, &song, &silent, 128);
        assert!(solo.is_pair());
        let [a, b] = solo.pair().unwrap();
        assert!((a.tele.az - 315.0).abs() < 0.5 && (b.tele.az - 45.0).abs() < 0.5, "左声道 {:.1}°，右声道 {:.1}°", a.tele.az, b.tele.az);
        assert!((solo.tele().az - 0.0).abs() < 0.5 || (solo.tele().az - 360.0).abs() < 0.5, "舞台正中间 {:.1}°", solo.tele().az);
        let side = 10.0 * (energy(&l[FS_USIZE..]) / energy(&r[FS_USIZE..])).log10();
        assert!((3.0..15.0).contains(&side), "只在左声道的内容：左耳比右耳响 {side:.1} dB");
        // 一次给 128 个样本和乱七八糟的长度，输出逐样本相同
        let mut again = Solo::new(cfg.clone(), PostMode::Realtime, 0.0).with_pair();
        let (mut l2, mut r2) = (vec![0f32; song.len()], vec![0f32; song.len()]);
        let (mut o, mut k) = (0, 0);
        while o < song.len() {
            let e = (o + [7usize, 333, 1500, 64][k % 4]).min(song.len());
            again.process(&song[o..e], &silent[o..e], &mut l2[o..e], &mut r2[o..e]);
            o = e;
            k += 1;
        }
        assert!(l.iter().zip(&l2).chain(r.iter().zip(&r2)).all(|(x, y)| x == y), "分块长度不该影响输出");
        // 立体声宽度拖到 0：一个声源在正前方，两耳一样响
        solo.set_config(Arc::new(EngineConfig::new(Params { stereo_width: 0.0, ..music.clone() }, None)));
        let (l, r) = run(&mut solo, &song, &silent, 128);
        assert!(!solo.is_pair() && solo.pair().is_none());
        let centre = 10.0 * (energy(&l[FS_USIZE..]) / energy(&r[FS_USIZE..])).log10();
        assert!(centre.abs() < 1.5, "合成一个声源后两耳应一样响：{centre:.1} dB");
        // 再拖回来；单声道的输入（麦克风）总是一个声源
        solo.set_config(cfg);
        run(&mut solo, &song, &silent, 128);
        assert!(solo.is_pair());
        let (mut a, mut b) = (vec![0f32; 4800], vec![0f32; 4800]);
        solo.process_mono(&song[..4800], &mut a, &mut b);
        assert!(!solo.is_pair());
    }

    /// 环绕（8D）时整个舞台一起转：左右两个声源始终相隔「立体声宽度」、一样远。
    #[test]
    fn the_stereo_stage_moves_as_one() {
        let p = Params { auto_level: false, mode: MotionMode::Orbit, orbit_period: 6.0, stereo_width: 60.0, ..Params::default().style_music() };
        let mut solo = Solo::new(Arc::new(EngineConfig::new(p, None)), PostMode::Realtime, 0.0);
        // 左右声道一响一轻：各自的音量不同，走位仍然一样
        let (a, b): (Vec<f32>, Vec<f32>) = (test_song(8.0, 5), test_song(8.0, 6).iter().map(|v| 0.2 * v).collect());
        let (mut l, mut r) = (vec![0f32; 480], vec![0f32; 480]);
        let (mut lo, mut hi) = (360f32, 0f32);
        for (ca, cb) in a.chunks_exact(480).zip(b.chunks_exact(480)) {
            solo.process(ca, cb, &mut l, &mut r);
            let [x, y] = solo.pair().unwrap();
            let gap = (y.tele.az - x.tele.az).rem_euclid(360.0);
            assert!((gap - 60.0).abs() < 0.5 && (x.tele.r - y.tele.r).abs() < 1e-4, "两个声源相隔 {gap:.1}°，距离 {} / {}", x.tele.r, y.tele.r);
            lo = lo.min(solo.tele().az);
            hi = hi.max(solo.tele().az);
        }
        assert!(lo < 20.0 && hi > 340.0, "8 秒里应当转过一整圈：{lo:.0}°…{hi:.0}°");
    }

    /// 和原声一样响：输出的响度跟着输入走——整体和输入一样响，轻的段落还是轻的（不像平时的自动响度那样被拉平），
    /// 一开头就差不多是对的；原声比目标还响时压到目标。
    #[test]
    fn level_match_keeps_the_loudness_and_dynamics_of_the_original() {
        let fs = FS_USIZE;
        // 12 秒：中间 4 秒轻 12 dB
        let shape = |x: Vec<f32>| -> Vec<f32> { x.into_iter().enumerate().map(|(i, v)| if (4 * fs..8 * fs).contains(&i) { 0.25 * v } else { v }).collect() };
        let (a, b) = (shape(test_song(12.0, 11)), shape(test_song(12.0, 12)));
        let sect = |l: &[f32], r: &[f32], s: usize| lufs(&l[s * fs + fs / 2..(s + 4) * fs - fs / 2], &r[s * fs + fs / 2..(s + 4) * fs - fs / 2]);
        let music = Params::default().style_music();
        let render = |p: Params, gain: f32| {
            let (x, y): (Vec<f32>, Vec<f32>) = (a.iter().map(|v| gain * v).collect(), b.iter().map(|v| gain * v).collect());
            let mut solo = Solo::new(Arc::new(EngineConfig::new(p, None)), PostMode::Realtime, 0.0);
            let (l, r) = run(&mut solo, &x, &y, 128);
            ((x, y), (l, r))
        };
        let ((x, y), (l, r)) = render(music.clone(), 1.0);
        let (in_all, out_all) = (lufs(&x, &y), lufs(&l, &r));
        assert!(in_all < music.target_lufs - 3.0, "测试信号应当比目标轻：{in_all:.1} LUFS");
        assert!((out_all - in_all).abs() < 1.5, "整体响度：输入 {in_all:.1}，输出 {out_all:.1} LUFS");
        // 各段：输出 − 输入 都差不多（强弱起伏保留）
        let d: Vec<f32> = [0, 4, 8].iter().map(|&s| sect(&l, &r, s) - sect(&x, &y, s)).collect();
        assert!(d.iter().all(|v| v.abs() < 2.0), "各段输出相对输入：{d:?} dB");
        assert!((sect(&l, &r, 0) - sect(&l, &r, 4) - 12.0).abs() < 2.0, "轻的那段应仍然轻约 12 dB：{:.1}", sect(&l, &r, 0) - sect(&l, &r, 4));
        // 开头 0.3–1.3 秒就已经和输入差不多响（从估计的增益起步，不用慢慢爬）
        let head = lufs(&l[fs * 3 / 10..fs * 13 / 10], &r[fs * 3 / 10..fs * 13 / 10]) - lufs(&x[fs * 3 / 10..fs * 13 / 10], &y[fs * 3 / 10..fs * 13 / 10]);
        assert!(head.abs() < 3.0, "开头输出相对输入 {head:.1} dB");
        // 对照：平时的自动响度会把轻的那段往上拉
        let (_, (l0, r0)) = render(Params { level_match: false, ..music.clone() }, 1.0);
        assert!(sect(&l0, &r0, 0) - sect(&l0, &r0, 4) < 9.0, "对照（拉到目标响度）轻的那段只轻 {:.1} dB", sect(&l0, &r0, 0) - sect(&l0, &r0, 4));
        // 原声比目标响（放大 20 dB）：输出压到目标附近，不跟着响上去
        let ((x, y), (l, r)) = render(music.clone(), 10.0);
        let loud_in = lufs(&x[8 * fs..], &y[8 * fs..]);
        let loud_out = lufs(&l[8 * fs..], &r[8 * fs..]);
        assert!(loud_in > music.target_lufs + 3.0 && (loud_out - music.target_lufs).abs() < 2.5, "原声 {loud_in:.1} LUFS，输出 {loud_out:.1}（目标 {}）", music.target_lufs);
    }
}
