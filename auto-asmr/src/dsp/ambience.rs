//! 从单声道录音估计持续存在的背景。时频掩模取原复频谱的分量，前景由延迟干声减去背景得到。

use realfft::{num_complex::Complex32, ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::{Arc, OnceLock};

const N: usize = 512;
const HOP: usize = N / 4;
pub const AMBIENCE_DELAY: usize = 2 * HOP;
const BINS: usize = N / 2 + 1;
// 六段共约 1.54 秒。保存每段的最低平滑功率，乐句中间短暂的气息不会立即变成背景。
const SECTION_HOPS: usize = 96;
const SECTIONS: usize = 6;
// 没有底噪谱时，只在开场约 0.2 秒用频谱形状辅助估计。
const STARTUP_HOPS: usize = 80;
type Plans = (Arc<dyn RealToComplex<f32>>, Arc<dyn ComplexToReal<f32>>);
static PLANS: OnceLock<Plans> = OnceLock::new();

struct NoiseBand {
    start: usize,
    end: usize,
    fast: f32,
    smooth: f32,
    quiet: f32,
    loud: f32,
    floor: f32,
    minima: [f32; SECTIONS],
    gain: f32,
}

impl NoiseBand {
    fn new(start: usize, end: usize) -> Self {
        Self { start, end, fast: 0.0, smooth: 0.0, quiet: 0.0, loud: 0.0, floor: 0.0, minima: [f32::INFINITY; SECTIONS], gain: 0.0 }
    }
}

pub struct AmbienceSeparator {
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    analysis_window: Vec<f32>,
    synthesis_window: Vec<f32>,
    input: Vec<f32>,
    overlap: Vec<f32>,
    time: Vec<f32>,
    spectrum: Vec<Complex32>,
    scratch: Vec<Complex32>,
    bands: Vec<NoiseBand>,
    head: usize,
    hops: usize,
    section: usize,
    silent_hops: usize,
    onset_power: f32,
    startup_rejection: f32,
}

impl AmbienceSeparator {
    pub fn new() -> Self {
        let (forward, inverse) = PLANS.get_or_init(|| {
            let mut planner = RealFftPlanner::new();
            (planner.plan_fft_forward(N), planner.plan_fft_inverse(N))
        }).clone();
        let scratch_len = forward.get_scratch_len().max(inverse.get_scratch_len());
        // 整帧分析，只合成最近两个步长。分析窗慢起快落，峰值落在合成段中央，
        // 让渐入底噪的估计及时跟上。两窗乘积为 Hann 窗，相邻两帧权重之和恒为 1。
        let analysis_window: Vec<_> = (0..N).map(|i| {
            let phase = if i < N - HOP {
                (i as f32 + 0.5) / (N - HOP) as f32
            } else {
                (N as f32 - i as f32 - 0.5) / HOP as f32
            };
            (std::f32::consts::FRAC_PI_2 * phase).sin()
        }).collect();
        let synthesis_window = (0..AMBIENCE_DELAY).map(|i| {
            let phase = std::f32::consts::PI * (i as f32 + 0.5) / AMBIENCE_DELAY as f32;
            phase.sin().powi(2) / (analysis_window[N - AMBIENCE_DELAY + i] * N as f32)
        }).collect();
        let mut bands = Vec::new();
        let mut start = 0;
        while start < BINS {
            // 低频保留窄带分辨率，高频合并约 1/6 倍频程，降低随机噪声造成的掩模抖动。
            let end = (start + (start / 8).max(1)).min(BINS);
            bands.push(NoiseBand::new(start, end));
            start = end;
        }
        Self {
            forward, inverse, analysis_window, synthesis_window, input: vec![0.0; N], overlap: vec![0.0; AMBIENCE_DELAY], time: vec![0.0; N],
            spectrum: vec![Complex32::default(); BINS], scratch: vec![Complex32::default(); scratch_len],
            bands, head: 0, hops: 0, section: 0, silent_hops: 0, onset_power: 0.0, startup_rejection: 0.0,
        }
    }

    /// 清掉旧录音和噪声估计，保留已分配的 FFT 及帧缓存。
    pub fn reset(&mut self) {
        self.input.fill(0.0);
        self.overlap.fill(0.0);
        self.head = 0;
        self.clear_estimate();
    }

    fn clear_estimate(&mut self) {
        self.hops = 0;
        self.section = 0;
        self.silent_hops = 0;
        self.onset_power = 0.0;
        self.startup_rejection = 0.0;
        for band in &mut self.bands {
            *band = NoiseBand::new(band.start, band.end);
        }
    }

    /// 返回（随声源运动的前景，留在房间的底响）。两路均延迟 AMBIENCE_DELAY 个采样。
    #[inline]
    pub fn process(&mut self, input: f32) -> (f32, f32) {
        let delayed = self.input[(self.head + N - AMBIENCE_DELAY) % N];
        let output_pos = self.head % AMBIENCE_DELAY;
        let bed = self.overlap[output_pos];
        self.overlap[output_pos] = 0.0;
        self.input[self.head] = input;
        self.head = (self.head + 1) % N;
        if self.head % HOP == 0 {
            self.analyze();
        }
        (delayed - bed, bed)
    }

    /// 在刚送入的原声里取一个分数延时样本。开关过渡时沿这条声轨移动读指针。
    pub(super) fn dry_at_delay(&self, delay: f32) -> f32 {
        let whole = delay as usize;
        let frac = delay - whole as f32;
        let past = |d: usize| self.input[(self.head + N - 1 - d) % N];
        let a = past(whole);
        let b = past(whole + 1);
        if whole == 0 {
            return a + frac * (b - a);
        }
        // 相邻四点的三次插值，减小读指针经过半个样本时对气声高频的衰减。
        let slope_a = 0.5 * (b - past(whole - 1));
        let slope_b = 0.5 * (past(whole + 2) - a);
        let delta = b - a;
        a + frac * (slope_a + frac * (3.0 * delta - 2.0 * slope_a - slope_b
            + frac * (slope_a + slope_b - 2.0 * delta)))
    }

    fn analyze(&mut self) {
        let ring = self.input[self.head..].iter().chain(&self.input[..self.head]);
        for ((sample, weight), x) in self.time.iter_mut().zip(&self.analysis_window).zip(ring) {
            *sample = x * weight;
        }
        self.forward.process_with_scratch(&mut self.time, &mut self.spectrum, &mut self.scratch).unwrap();

        let mut frame_power = 0.0;
        let mut strongest_bin = 0.0f32;
        for band in &mut self.bands {
            let bins = &self.spectrum[band.start..band.end];
            let mut energy_now = 0.0;
            for bin in bins {
                let power = bin.norm_sqr();
                strongest_bin = strongest_bin.max(power);
                energy_now += power;
            }
            frame_power += energy_now;
            let power = energy_now / bins.len() as f32;
            // 约 7 ms 的检测负责保护起音，约 36 ms 的平滑功率用于估计噪声底。
            band.fast += 0.32 * (power - band.fast);
            band.smooth += 0.072 * (power - band.smooth);
            // 慢起的低包络与慢落的高包络若相近，频带在近几百毫秒内都比较稳定。
            band.quiet = band.smooth.min(band.quiet + 0.012 * (band.smooth - band.quiet));
            band.loud = band.smooth.max(band.loud * 0.993);
        }
        // 一次门限闭合可能正好隔开两个音节。沿用上次观察到的房间底声，
        // 否则开麦后的第一口气会被拿来初始化整个底噪谱。静音超过估计窗口才按新片段处理。
        if frame_power < 1e-15 {
            self.silent_hops = self.silent_hops.saturating_add(1);
            if self.hops > 0 && self.silent_hops == SECTION_HOPS * SECTIONS {
                self.clear_estimate();
            }
            return;
        }
        self.silent_hops = 0;
        // 有效帧开始时立即判断。宽带、低峰值密度而且没有明显起音的声音可以直接为噪声底提供证据。
        // 突出的谱线保护元音；相对近期响度突然上升时保护短促气息和辅音。
        let peak_share = strongest_bin / frame_power;
        let attack = self.hops > 5 && frame_power > 5.0 * self.onset_power;
        let rise = self.hops > 5 && frame_power > 2.0 * self.onset_power;
        self.onset_power += 0.08 * (frame_power - self.onset_power);
        if self.hops < 5 {
            self.onset_power = frame_power;
        }
        // 6 kHz 以上的能量，以及 2.25 kHz 以下语音主体的占比，区分嘶声/气息与宽带背景。
        let top_energy: f32 = self.spectrum[64..].iter().map(|z| z.norm_sqr()).sum();
        let body_share = self.spectrum[..24].iter().map(|z| z.norm_sqr()).sum::<f32>() / frame_power;
        let high_share = top_energy / frame_power;
        // 极弱编码前导的相对增幅也很大，只有可听的强起音才作为突发音。
        if frame_power > 0.01 && (attack || (rise && high_share > 0.4)) {
            self.startup_rejection = 1.0;
        } else {
            self.startup_rejection *= 0.982;
        }
        let tonal = ((peak_share - 0.24) / 0.29).clamp(0.0, 1.0);
        // 机箱、风扇或电源嗡声可以有几条相邻的低频谱线。留出窗函数主瓣的宽度。
        let low_hum = self.spectrum[..5].iter().map(|z| z.norm_sqr()).sum::<f32>() > 0.97 * frame_power;
        let air_is_voice = high_share > 0.88 || (body_share < 0.025 && high_share < 0.55);
        let voiced = high_share < 0.015 && !low_hum;
        let evidence = if air_is_voice || voiced { 0.0 } else { 1.0 - tonal };
        let opening = self.hops < STARTUP_HOPS;
        let background = if opening { evidence * (1.0 - self.startup_rejection) } else { 0.0 };
        let broad_shape = ((high_share - 0.035) / 0.035).clamp(0.0, 1.0)
            * ((body_share - 0.10) / 0.15).clamp(0.0, 1.0);
        // 淡入或话筒增益改变会使许多频带一起升高。用功率比的下四分位估计共同增幅，
        // 这样少数新出现的谐波抬不起整个底噪谱。复用两次 FFT 之间空闲的时域缓冲。
        let mut count = 0;
        if !opening && evidence > 0.5 && self.startup_rejection < 0.15 {
            for band in &self.bands {
                if band.floor > (1e-7 * frame_power).max(1e-18) {
                    self.time[count] = band.smooth / band.floor;
                    count += 1;
                }
            }
        }
        let common_rise = if count >= 8 {
            let (_, ratio, _) = self.time[..count].select_nth_unstable_by(count / 4, f32::total_cmp);
            ratio.clamp(1.0, 1.06)
        } else { 1.0 };
        {
            for i in 0..self.bands.len() {
                // 启动时没有纯底噪样本。用相邻频带限制初始估计，防止开头的稳定元音被当成底噪。
                let lower = self.bands[i.saturating_sub(2)..i].iter();
                let upper = self.bands[(i + 1)..(i + 3).min(self.bands.len())].iter();
                let shoulder = lower.chain(upper).map(|b| b.smooth).fold(f32::INFINITY, f32::min);
                let band = &mut self.bands[i];
                band.floor *= common_rise;
                band.minima[self.section] = band.minima[self.section].min(band.smooth);
                // 窄带的随机波动较大，给它的最小统计量补偿也稍大。
                let bias = 1.15 + 0.9 / (band.end - band.start) as f32;
                let observed = band.minima.iter().copied().fold(f32::INFINITY, f32::min) * bias;
                let smoothness = (8.0 * shoulder / band.smooth.max(1e-24)).clamp(0.0, 1.0);
                let startup = if opening && low_hum && band.start < 5 {
                    1.0
                } else { background * smoothness.max(broad_shape) };
                // 用本频带约几百毫秒的低包络验证新的底噪。高频嘶声必须经过这段观察，
                // 低频嗡声则只允许低频带走捷径，避免把同时出现的轻声也写进底噪谱。
                let stable = band.quiet > 0.52 * band.loud && !opening;
                let harmonic = voiced && band.smooth > 2.0 * shoulder;
                let stable_floor = if stable && !harmonic { band.quiet } else { 0.0 };
                let target_floor = observed.max(stable_floor);
                if self.hops == 0 || band.floor <= 1e-16 {
                    band.floor = target_floor.min(0.5 * shoulder);
                } else if band.smooth < band.floor {
                    band.floor += 0.10 * (band.smooth - band.floor);
                } else if stable_floor > band.floor {
                    band.floor += 0.035 * (stable_floor - band.floor);
                } else if !harmonic && target_floor > band.floor {
                    // 安静录音的底噪可以重新学习。强且窄的谐波处每秒最多上升约 1 dB。
                    let cap = if observed > 1.5 * shoulder { 1.0006 } else { 1.008 };
                    band.floor = target_floor.min(band.floor * cap);
                }
                // 用可信的宽带开场跨过从静音到房间声的淡入。只有持续的分量才能写入噪声底。
                if startup > 0.65 {
                    band.floor = band.floor.max(startup * band.smooth);
                }
                let excess = (0.6 * band.fast + 0.4 * band.smooth) / band.floor.max(1e-18);
                // 背景占主导时整带留在房间。新出现的能量高于噪声底后迅速交还人声支路。
                // 短时功率在窄带噪声中波动较大。慢功率仍贴近噪声谱时允许这种波动，
                // 只要慢功率也升高，就恢复对新音节的灵敏度。
                let near_floor = (2.5 - band.smooth / band.floor.max(1e-18)).clamp(0.0, 1.0);
                let threshold = 1.7 + near_floor;
                let tracked = if opening && (air_is_voice || voiced) { 0.0 } else {
                    (threshold / excess.max(threshold)).powi(2)
                };
                let gain = tracked.max(startup);
                let follow = if self.hops < 4 { 1.0 } else if gain < band.gain { 0.7 } else { 0.15 + 0.7 * startup };
                band.gain += follow * (gain - band.gain);
            }
        }
        self.hops = self.hops.saturating_add(1);
        if self.hops % SECTION_HOPS == 0 {
            self.section = (self.section + 1) % SECTIONS;
            for band in &mut self.bands {
                band.minima[self.section] = f32::INFINITY;
            }
        }
        // 在相邻频带中心间线性插值，避免频谱里出现阶梯。
        let mut left_bin = 0;
        let mut left_gain = self.bands[0].gain;
        for band in &self.bands {
            let center = (band.start + band.end - 1) / 2;
            let step = (band.gain - left_gain) / (center - left_bin).max(1) as f32;
            for (offset, bin) in self.spectrum[left_bin..center].iter_mut().enumerate() {
                *bin *= left_gain + offset as f32 * step;
            }
            left_gain = band.gain;
            left_bin = center;
        }
        for bin in &mut self.spectrum[left_bin..] {
            *bin *= left_gain;
        }
        self.inverse.process_with_scratch(&mut self.spectrum, &mut self.time, &mut self.scratch).unwrap();
        for (i, (&sample, &weight)) in self.time[N - AMBIENCE_DELAY..].iter().zip(&self.synthesis_window).enumerate() {
            self.overlap[(self.head + i) % AMBIENCE_DELAY] += sample * weight;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::rng::Rng;

    #[test]
    fn stationary_noise_is_separated_and_stems_reconstruct_the_delayed_input() {
        let mut splitter = AmbienceSeparator::new();
        let mut rng = Rng::new(5);
        let mut input = vec![0.0; 3 * 48000];
        let mut residual = 0.0;
        let mut total = 0.0;
        for i in 0..input.len() {
            input[i] = rng.uniform(-0.01, 0.01);
            let (voice, bed) = splitter.process(input[i]);
            let delayed = if i >= AMBIENCE_DELAY { input[i - AMBIENCE_DELAY] } else { 0.0 };
            assert!((voice + bed - delayed).abs() < 1e-8);
            if i > 48000 {
                residual += voice * voice;
                total += delayed * delayed;
            }
        }
        assert!(residual < total * 0.02, "底噪剩在移动声源中的能量比例 {}", residual / total);
        splitter.reset();
        for _ in 0..2 * N {
            assert_eq!(splitter.process(0.0), (0.0, 0.0));
        }
    }

    #[test]
    fn speech_and_short_quiet_breath_stay_in_the_moving_stem() {
        let mut splitter = AmbienceSeparator::new();
        let mut rng = Rng::new(11);
        let mut last = 0.0;
        let mut signal = vec![0.0; 5 * 48000];
        let mut desired = [0.0; 2];
        let mut leaked = [0.0; 2];
        for i in 0..signal.len() {
            let t = i as f32 / 48000.0;
            let wind = rng.uniform(-1.0, 1.0);
            let noise = rng.uniform(-0.0015, 0.0015);
            if (1.0..3.8).contains(&t) {
                let phase = std::f32::consts::TAU * 183.0 * t;
                signal[i] = 0.035 * (phase.sin() + 0.45 * (phase * 3.0).sin());
            } else if (4.15..4.45).contains(&t) {
                let env = (std::f32::consts::PI * (t - 4.15) / 0.3).sin().powi(2);
                signal[i] = 0.008 * env * (wind - last);
            }
            last = wind;
            let (moving, _) = splitter.process(noise + signal[i]);
            if i < AMBIENCE_DELAY { continue; }
            let s = signal[i - AMBIENCE_DELAY];
            if s != 0.0 {
                let kind = usize::from(i > 4 * 48000);
                desired[kind] += s * s;
                leaked[kind] += (s - moving) * (s - moving);
            }
        }
        assert!(leaked[0] < desired[0] * 0.02, "持续人声误差 {}", leaked[0] / desired[0]);
        assert!(leaked[1] < desired[1] * 0.15, "轻气息误差 {}", leaked[1] / desired[1]);
    }

    #[test]
    fn opening_hiss_and_later_quiet_voice_stay_in_front() {
        let mut rng = Rng::new(156);
        let mut splitter = AmbienceSeparator::new();
        let mut hiss_filter = crate::dsp::filters::Biquad::bandpass(4600.0, 2.0);
        let mut hiss_filter2 = hiss_filter;
        let mut energies = [[0.0; 2]; 2];
        for i in 0..4 * 48000 {
            let t = i as f32 / 48000.0;
            let noise = rng.uniform(-1.0, 1.0);
            let x = if t < 0.25 {
                // 起音逐渐增强、带有共振的气息/清辅音。
                let envelope = (t / 0.035).min(1.0) * ((0.25 - t) / 0.07).min(1.0);
                0.008 * envelope * hiss_filter2.process(hiss_filter.process(noise))
            } else if t < 2.0 {
                0.025 * noise
            } else {
                let fade = (1.0 - (t - 2.0) / 0.4).max(0.001);
                let angle = std::f32::consts::TAU * 183.0 * t;
                let voice = if t > 2.8 { 0.0015 * (angle.sin() + 0.5 * (3.0 * angle).sin()) } else { 0.0 };
                0.025 * fade * noise + voice
            };
            let (voice, bed) = splitter.process(x);
            let section = if (0.04..0.22).contains(&t) { Some(0) } else if t > 3.0 { Some(1) } else { None };
            if let Some(k) = section {
                energies[k][0] += (voice + bed).powi(2);
                energies[k][1] += bed * bed;
            }
        }
        for (i, [original, leakage]) in energies.into_iter().enumerate() {
            assert!(leakage < original * 0.12, "起音 {i} 泄漏到底声的功率比例 {}", leakage / original);
        }
    }

    #[test]
    fn learns_room_after_a_quiet_edit_and_protects_opening_voice() {
        let mut rng = Rng::new(2541);
        // 数字静音后先出现很弱的编码前导，再渐入实际的有色底声。量第一段可听的 90 ms。
        let mut opening = AmbienceSeparator::new();
        let mut low = 0.0;
        let (mut residual, mut original) = (0.0, 0.0);
        for i in 0..24000 {
            let t = i as f32 / 48000.0;
            let noise = rng.uniform(-1.0, 1.0);
            low += 0.035 * (noise - low);
            let gain = if t < 0.20 { 0.0 } else if t < 0.235 { 2e-7 } else { 0.0002 * ((t - 0.235) / 0.06).min(1.0) };
            let (moving, room) = opening.process(gain * (0.4 * noise + 2.0 * low));
            if (0.26..0.35).contains(&t) {
                residual += moving * moving;
                original += (moving + room).powi(2);
            }
        }
        assert!(residual < original * 0.022, "首段渐入底声的移动残留功率比例 {}", residual / original);
        for initial_noise in [0.0, 1e-6] {
            let mut separator = AmbienceSeparator::new();
            let (mut voice_energy, mut bed_energy) = (0.0, 0.0);
            for i in 0..3 * 48000 {
                let scale = if i < 2 * 48000 { initial_noise } else { 0.008 };
                let (voice, bed) = separator.process(rng.uniform(-scale, scale));
                if i > 2 * 48000 + 33600 {
                    voice_energy += voice * voice;
                    bed_energy += bed * bed;
                }
            }
            assert!(voice_energy < 0.035 * bed_energy, "剪辑后 0.7–1 秒的残留 {}", voice_energy / bed_energy);
        }
        let mut separator = AmbienceSeparator::new();
        let (mut input_energy, mut bed_energy) = (0.0, 0.0);
        for i in 0..3 * 48000 {
            let t = i as f32 / 48000.0;
            let angle = std::f32::consts::TAU * 180.0 * t;
            let x = 0.03 * (angle.sin() + 0.45 * (3.0 * angle).sin() + 0.20 * (7.0 * angle).sin());
            let (_, bed) = separator.process(x);
            input_energy += x * x;
            bed_energy += bed * bed;
        }
        assert!(bed_energy < 0.03 * input_energy, "开头持续元音泄漏 {}", bed_energy / input_energy);
    }

    #[test]
    fn short_mic_gate_preserves_the_next_breath() {
        let mut split = AmbienceSeparator::new();
        let mut rng = Rng::new(428);
        let mut air = crate::dsp::filters::Biquad::bandpass(1800.0, 0.7);
        let mut target = vec![0.0; 3 * 48000];
        let (mut target_power, mut projection) = (0.0, 0.0);
        // 先听两秒房间声，门限关 240 ms，麦克风再次打开时说话人轻轻送气。
        let start = 96000 + 11520;
        for i in 0..target.len() {
            let room = 0.00012 * rng.uniform(-1.0, 1.0);
            let breath = air.process(rng.uniform(-1.0, 1.0));
            if (start..start + 12000).contains(&i) {
                let phase = (i - start) as f32 / 12000.0;
                target[i] = 0.006 * (std::f32::consts::PI * phase).sin().powi(2) * breath;
            }
            let input = if (96000..start).contains(&i) { 0.0 } else { room + target[i] };
            let (moving, _) = split.process(input);
            if (start + AMBIENCE_DELAY..start + AMBIENCE_DELAY + 12000).contains(&i) {
                let voice = target[i - AMBIENCE_DELAY];
                target_power += voice * voice;
                projection += voice * moving;
            }
        }
        let gain = projection / target_power;
        assert!(gain > 0.85, "短暂关麦后气声振幅保留 {gain:.3}");

        // 长静音后接另一段录音，较响的房间声仍要能从开场固定下来。
        for _ in 0..2 * 48000 {
            split.process(0.0);
        }
        let (mut residual, mut total) = (0.0, 0.0);
        for i in 0..12000 {
            let (moving, fixed) = split.process(0.004 * rng.uniform(-1.0, 1.0));
            if i > AMBIENCE_DELAY + 1200 {
                residual += moving * moving;
                total += (moving + fixed).powi(2);
            }
        }
        assert!(residual < 0.05 * total, "长静音后新底噪的移动残留 {}", residual / total);
    }

    // 先量单独底噪在移动支路的残留，再用混合段与已知干净前景的投影量前景增益。
    // 从移动支路减去干净前景，剩下的背景泄漏和前景失真都计入误差。
    fn measure_mixed_stems(noise: &[f32], target: &[f32], active: std::ops::Range<usize>) -> (f64, f64, f64) {
        let mut separator = AmbienceSeparator::new();
        let (mut quiet_noise, mut quiet_leak) = (0.0, 0.0);
        let (mut mixed_noise, mut mixed_error, mut target_power, mut correlation) = (0.0, 0.0, 0.0, 0.0);
        for i in 0..noise.len() {
            let (moving, _) = separator.process(noise[i] + target[i]);
            if i < AMBIENCE_DELAY { continue; }
            let j = i - AMBIENCE_DELAY;
            let noise = f64::from(noise[j]);
            let target = f64::from(target[j]);
            let moving = f64::from(moving);
            if (48000..active.start).contains(&j) {
                quiet_noise += noise * noise;
                quiet_leak += moving * moving;
            } else if active.contains(&j) {
                mixed_noise += noise * noise;
                mixed_error += (moving - target).powi(2);
                target_power += target * target;
                correlation += moving * target;
            }
        }
        (quiet_leak / quiet_noise, mixed_error / mixed_noise, correlation / target_power)
    }

    #[test]
    fn sustained_high_hiss_stays_fixed_under_a_short_breath() {
        let mut rng = Rng::new(931);
        let mut breath_band = crate::dsp::filters::Biquad::bandpass(4200.0, 1.6);
        let (mut prev, mut older) = (0.0, 0.0);
        let mut noise = vec![0.0; 3 * 48000];
        let mut breath = vec![0.0; noise.len()];
        let active = 96000..108000;
        for i in 0..noise.len() {
            let white = rng.uniform(-1.0, 1.0);
            noise[i] = 0.015 * (white - 2.0 * prev + older);
            older = prev;
            prev = white;
            let air = breath_band.process(rng.uniform(-1.0, 1.0));
            if active.contains(&i) {
                let phase = (i - active.start) as f32 / active.len() as f32;
                breath[i] = 0.09 * (std::f32::consts::PI * phase).sin().powi(2) * air;
            }
        }
        let (quiet, mixed, gain) = measure_mixed_stems(&noise, &breath, active);
        assert!(quiet < 0.12 && mixed < 0.30 && gain > 0.65,
            "持续嘶声残留 {quiet:.3}，气声段误差/底噪 {mixed:.3}，气声振幅保留 {gain:.3}");
    }

    #[test]
    fn low_motor_harmonics_stay_fixed_as_the_level_rises() {
        let mut splitter = AmbienceSeparator::new();
        let mut rng = Rng::new(240);
        let mut phase = 0.0f32;
        let mut energy = [[0.0; 2]; 2];
        for i in 0..3 * 48000 {
            let t = i as f32 / 48000.0;
            phase += std::f32::consts::TAU * (48.0 + 0.2 * t.sin()) / 48000.0;
            let level = 0.025 * (1.0 + (t - 1.0).clamp(0.0, 1.0));
            let motor = 0.15 * phase.sin() + 0.42 * (3.0 * phase).sin() + (5.0 * phase).sin();
            let (moving, fixed) = splitter.process(level * (motor + rng.uniform(-0.02, 0.02)));
            let window = if i < 12000 { Some(0) } else if i >= 48000 { Some(1) } else { None };
            if let Some(k) = window {
                energy[k][0] += moving * moving;
                energy[k][1] += (moving + fixed).powi(2);
            }
        }
        for [leak, input] in energy {
            assert!(leak < 0.05 * input, "电机嗡声留在移动路的功率比例 {}", leak / input);
        }
    }

    #[test]
    fn weak_modulated_voice_stays_in_front_of_loud_fan_noise() {
        let mut rng = Rng::new(125);
        let mut low = 0.0;
        let mut noise = vec![0.0; 5 * 48000];
        let mut voice = vec![0.0; noise.len()];
        let active = 96000..204000;
        for i in 0..noise.len() {
            low += 0.035 * (rng.uniform(-1.0, 1.0) - low);
            noise[i] = 0.14 * low;
            if active.contains(&i) {
                let t = (i - active.start) as f32 / 48000.0;
                let phase = std::f32::consts::TAU * (190.0 * t + 0.8 * (std::f32::consts::TAU * 1.4 * t).sin());
                let syllable = (std::f32::consts::TAU * 2.0 * t).sin().max(0.0);
                voice[i] = 0.018 * syllable * (phase.sin() + 0.4 * (3.0 * phase).sin() + 0.2 * (5.0 * phase).sin());
            }
        }
        let (quiet, mixed, gain) = measure_mixed_stems(&noise, &voice, active);
        assert!(quiet < 0.12 && mixed < 0.35 && gain > 0.50,
            "风声残留 {quiet:.3}，发声段误差/底噪 {mixed:.3}，弱人声振幅保留 {gain:.3}");
    }
}
