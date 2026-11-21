//! 从单声道录音估计持续存在的背景。时频掩模取原复频谱的分量，前景由延迟干声减去背景得到。

use realfft::{num_complex::Complex32, ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::{Arc, OnceLock};

pub const AMBIENCE_DELAY: usize = 512;
const N: usize = AMBIENCE_DELAY;
const HOP: usize = N / 4;
const BINS: usize = N / 2 + 1;
// 六段共约 1.54 秒。保存每段的最低平滑功率，乐句中间短暂的气息不会立即变成背景。
const SECTION_HOPS: usize = 96;
const SECTIONS: usize = 6;
// 开场缺少噪声样本：用最初约 1.5 s 的声学证据填好整个最小统计量窗口。
const STARTUP_HOPS: usize = SECTIONS * SECTION_HOPS;
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
    window: Vec<f32>,
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
    room_power: f32,
    room_observations: usize,
}

impl AmbienceSeparator {
    pub fn new() -> Self {
        let (forward, inverse) = PLANS.get_or_init(|| {
            let mut planner = RealFftPlanner::new();
            (planner.plan_fft_forward(N), planner.plan_fft_inverse(N))
        }).clone();
        let scratch_len = forward.get_scratch_len().max(inverse.get_scratch_len());
        // 周期性正弦窗，四帧重叠时窗平方之和为 2。
        let window = (0..N).map(|i| (std::f32::consts::PI * (i as f32 + 0.5) / N as f32).sin()).collect();
        let mut bands = Vec::new();
        let mut start = 0;
        while start < BINS {
            // 低频保留窄带分辨率，高频合并约 1/6 倍频程，降低随机噪声造成的掩模抖动。
            let end = (start + (start / 8).max(2)).min(BINS);
            bands.push(NoiseBand::new(start, end));
            start = end;
        }
        Self {
            forward, inverse, window, input: vec![0.0; N], overlap: vec![0.0; N], time: vec![0.0; N],
            spectrum: vec![Complex32::default(); BINS], scratch: vec![Complex32::default(); scratch_len],
            bands, head: 0, hops: 0, section: 0, silent_hops: 0, onset_power: 0.0, startup_rejection: 0.0, room_power: 0.0, room_observations: 0,
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
        self.room_power = 0.0;
        self.room_observations = 0;
        for band in &mut self.bands {
            *band = NoiseBand::new(band.start, band.end);
        }
    }

    /// 返回（随声源运动的前景，留在房间的底响）。两路均延迟 AMBIENCE_DELAY 个采样。
    #[inline]
    pub fn process(&mut self, input: f32) -> (f32, f32) {
        let delayed = self.input[self.head];
        let bed = self.overlap[self.head];
        self.overlap[self.head] = 0.0;
        self.input[self.head] = input;
        self.head = (self.head + 1) % N;
        if self.head % HOP == 0 {
            self.analyze();
        }
        (delayed - bed, bed)
    }

    fn analyze(&mut self) {
        let ring = self.input[self.head..].iter().chain(&self.input[..self.head]);
        for ((sample, weight), x) in self.time.iter_mut().zip(&self.window).zip(ring) {
            *sample = x * weight;
        }
        self.forward.process_with_scratch(&mut self.time, &mut self.spectrum, &mut self.scratch).unwrap();

        let mut frame_power = 0.0;
        let mut strongest_bin = 0.0f32;
        let mut total = 0.0;
        let mut stable = 0.0;
        let mut largest = 0.0f32;
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
            let energy = band.smooth * bins.len() as f32;
            total += energy;
            largest = largest.max(energy);
            if band.quiet > 0.6 * band.loud { stable += energy; }
        }
        // 跳过无信号的帧。静音不能作为房间的功率估计，重新开始播放也要重新判断开场。
        if frame_power < 1e-15 {
            self.silent_hops += 1;
            if self.silent_hops == 32 {
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
        // 从编解码器的极小前导 / 数字静音渐入时，相对增幅也很大；
        // 至少已有可用的噪声标尺，或确实出现强起音，才按突发音处理。
        let established = self.room_power > 0.001 || frame_power > 0.01;
        if established && ((attack && frame_power > 6.0 * self.room_power) || (rise && high_share > 0.4)) {
            self.startup_rejection = 1.0;
        } else {
            self.startup_rejection *= 0.982;
        }
        // 窄频、缓慢变大的持续低频（如空调声）也常伴随可见的宽带声底。
        let tonal = ((peak_share - 0.24) / 0.29).clamp(0.0, 1.0);
        let low_hum = self.spectrum[..2].iter().map(|z| z.norm_sqr()).sum::<f32>() > 0.95 * frame_power;
        let air_is_voice = high_share > 0.88 || (body_share < 0.025 && high_share < 0.55);
        let voiced = high_share < 0.015 && !low_hum;
        let evidence = if air_is_voice || voiced { 0.0 } else if low_hum { 1.0 } else { 1.0 - tonal };
        if self.hops < STARTUP_HOPS && evidence > 0.7 && self.startup_rejection < 0.5 {
            self.room_observations += 1;
            if self.room_observations > 5 {
                self.room_power = self.room_power.max(self.onset_power);
            }
        }
        if self.room_power > self.onset_power {
            self.room_power += 0.002 * (self.onset_power - self.room_power);
        }
        let room_match = ((6.0 - frame_power / self.room_power.max(1e-20)) / 3.0).clamp(0.0, 1.0);
        let onset_room = if self.hops < STARTUP_HOPS { evidence } else { 0.0 };
        let close_to_room = if self.startup_rejection > 0.15 || air_is_voice || voiced { 0.0 } else { room_match * (1.0 - tonal.powi(4)) };
        let background = onset_room.max(close_to_room) * (1.0 - self.startup_rejection);
        let broad_shape = ((high_share - 0.035) / 0.035).clamp(0.0, 1.0)
            * ((body_share - 0.10) / 0.15).clamp(0.0, 1.0);
        // 数字静音或接近静音之后，新出现且持续的宽带底响可重新建立噪声底。
        // 若能量集中在单个谐波或各带都在起伏，仍需等待长期最低功率作为证据。
        let steady_broadband = total > 1e-12 && stable > 0.75 * total && largest < 0.35 * total;
        {
            let threshold = if steady_broadband { 3.0 } else { 1.6 };
            for i in 0..self.bands.len() {
                // 启动时没有纯底噪样本。用相邻频带限制初始估计，防止开头的稳定元音被当成底噪。
                let lower = self.bands[i.saturating_sub(2)..i].iter();
                let upper = self.bands[(i + 1)..(i + 3).min(self.bands.len())].iter();
                let shoulder = lower.chain(upper).map(|b| b.smooth).fold(f32::INFINITY, f32::min);
                let band = &mut self.bands[i];
                band.minima[self.section] = band.minima[self.section].min(band.smooth);
                // 窄带的随机波动较大，给它的最小统计量补偿也稍大。
                let bias = 1.15 + 0.9 / (band.end - band.start) as f32;
                let observed = band.minima.iter().copied().fold(f32::INFINITY, f32::min) * bias;
                let stable_floor = if steady_broadband { band.quiet.min(2.0 * shoulder) } else { 0.0 };
                let target_floor = observed.max(stable_floor);
                if self.hops == 0 || band.floor <= 1e-16 {
                    band.floor = target_floor.min(0.5 * shoulder);
                } else if target_floor < band.floor {
                    band.floor += 0.10 * (target_floor - band.floor);
                } else if stable_floor > band.floor {
                    band.floor += 0.035 * (stable_floor - band.floor);
                } else {
                    // 安静录音的底噪可以重新学习。强且窄的谐波处每秒最多上升约 1 dB。
                    let cap = if observed > 1.5 * shoulder { 1.0006 } else { 1.008 };
                    band.floor = target_floor.min(band.floor * cap);
                }
                let smoothness = (8.0 * shoulder / band.smooth.max(1e-24)).clamp(0.0, 1.0);
                let startup = background * if low_hum { 1.0 } else { smoothness.max(close_to_room).max(broad_shape) };
                // 用可信的宽带开场跨过从静音到房间声的淡入。只有持续的分量才能写入噪声底。
                if startup > 0.65 {
                    band.floor = band.floor.max(startup * band.smooth);
                    band.minima[self.section] = band.minima[self.section].max(startup * band.quiet);
                }
                let excess = band.fast / band.floor.max(1e-18);
                // 背景占主导时整带留在房间。新出现的能量高于噪声底后迅速交还人声支路。
                let gain = if air_is_voice {
                    0.0
                } else {
                    let harmonic_limit = if voiced { smoothness.powi(2) } else { 1.0 };
                    ((threshold / excess.max(threshold)).powi(2) * harmonic_limit).max(startup)
                };
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
        for i in 0..N {
            self.overlap[(self.head + i) % N] += self.time[i] * self.window[i] * (0.5 / N as f32);
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
            let delayed = if i >= N { input[i - N] } else { 0.0 };
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
            if i < N { continue; }
            let s = signal[i - N];
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
}
