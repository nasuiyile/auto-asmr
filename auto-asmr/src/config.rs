//! 设置文件：桌面版和安卓版共用同一个格式，互相导入导出。
//!
//! 桌面版关闭时自动保存、「导出配置」写出的都是 eframe 的键值表 `{"app": "<Saved 的 RON>"}`；
//! 也接受直接写的 `Saved`（RON）。缺的项一律用默认值，所以旧文件、只写了几项的文件都能读。
//!
//! 安卓版读写同一个格式：用得上的是 `params`（全部效果参数）和 `dry`（输出效果 / 监听原声）；
//! 音频设备、缓冲、文件转换这些安卓用不上的设置原样保留，导出后带回桌面版照样能用。

use crate::dsp::params::Params;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 设置在键值表里的键（就是 `eframe::APP_KEY`，桌面版的测试核对两者相同）。
pub const APP_KEY: &str = "app";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
/// 桌面版界面打开时停在哪个页签。
pub enum Tab {
    Realtime,
    File,
}

/// 关闭程序时保存、下次打开时恢复的内容（桌面版的全部设置）。安卓版只用 `params` 和 `dry`。
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub params: Params,
    pub english: bool,
    pub tab: Tab,
    pub host: String,
    pub input: Option<String>,
    pub output: Option<String>,
    pub buffer: u32,
    pub in_channel: usize,
    pub dry: bool,
    pub bits: u16,
    pub file_lufs: f32,
    pub file_channel: u8,
    /// 批量转换同时处理几个文件；0 = 自动（CPU 逻辑核数）。
    pub jobs: usize,
    /// 批量转换的输出文件夹；None = 放在各自源文件旁边。
    pub out_dir: Option<PathBuf>,
    /// 是否显示 3D 观察窗（默认隐藏；隐藏时不绘制）。
    pub view3d: bool,
    /// 多音频合成：列表里的前几个音频（最多 [`crate::dsp::mix::MAX_VOICES`] 个）各放一个位置，同时播放、合成一个文件。
    pub mix: bool,
    /// 合成的排布：false = 各自固定在自己的位置；true = 一起移动（按"左右移动"的方式走，彼此错开 360°/N）。
    pub mix_follow: bool,
    /// 合成前自动对齐：同一首歌的几个版本，从一起开始唱的地方叠加，对不上的开头和结尾裁掉。
    pub mix_align: bool,
    /// 合成的自动平衡强度（0–100 %，0 = 关）：按短时响度让几段每一段时间都差不多一样响。
    pub mix_balance: f32,
    /// 左右双人（0 = 关）：一个音频当成两个人，同一个声音一左一右，第 2 个晚这么多毫秒。排布、自动平衡与多音频合成共用。
    pub duo_ms: f32,
    /// 实时（主播）模式的左右双人（0 = 关）：麦克风的声音一左一右两个人说，第 2 个晚这么多毫秒。和文件转换的
    /// `duo_ms` 分开记（直播用不用和转换文件用不用互不影响）；两个声音的位置、排布与文件转换的左右双人共用。
    pub rt_duo_ms: f32,
}

impl Default for Saved {
    fn default() -> Self {
        Saved { params: Params::default(), english: false, tab: Tab::Realtime, host: String::new(), input: None, output: None, buffer: 256, in_channel: 0, dry: false, bits: 24, file_lufs: -16.0, file_channel: 0, jobs: 0, out_dir: None, view3d: false, mix: false, mix_follow: false, mix_align: false, mix_balance: 0.0, duo_ms: 0.0, rt_duo_ms: 0.0 }
    }
}

/// 读配置文件出错的原因。
#[derive(Debug)]
pub enum ConfigError {
    /// 是键值表，但里面没有 auto-asmr 的设置。
    NotAutoAsmr,
    /// 读不懂（RON 语法或类型不对）。
    Parse(String),
}

/// 读取配置文件：键值表格式（自动保存 / 导出配置）或直接写的 `Saved`。缺的项用默认值。
pub fn parse(text: &str) -> Result<Saved, ConfigError> {
    if let Ok(map) = ron::from_str::<std::collections::BTreeMap<String, String>>(text) {
        let inner = map.get(APP_KEY).ok_or(ConfigError::NotAutoAsmr)?;
        return ron::from_str::<Saved>(inner).map_err(|e| ConfigError::Parse(e.to_string()));
    }
    ron::from_str::<Saved>(text).map_err(|e| ConfigError::Parse(e.to_string()))
}

/// 写成配置文件（键值表格式，与桌面版自动保存、导出配置相同）。
pub fn to_text(s: &Saved) -> Result<String, String> {
    let inner = ron::ser::to_string_pretty(s, Default::default()).map_err(|e| e.to_string())?;
    let values = std::collections::BTreeMap::from([(APP_KEY, inner)]);
    ron::ser::to_string_pretty(&values, Default::default()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_text_reads_back_and_keeps_desktop_only_settings() {
        let s = Saved { buffer: 512, input: Some("Mic".into()), dry: true, rt_duo_ms: 90.0, params: Params { puff: 2.5, ..Params::default() }, ..Saved::default() };
        let back = parse(&to_text(&s).unwrap()).unwrap();
        assert_eq!(back.params, s.params);
        assert!(back.dry && back.buffer == 512 && back.input.as_deref() == Some("Mic") && back.rt_duo_ms == 90.0);
        // 只写了参数的文件（比如手写的）也能读，其余用默认值
        let bare = parse("(params: (seed: 3))").unwrap();
        assert_eq!(bare.params.seed, 3);
        assert_eq!(bare.buffer, 256);
        assert_eq!(bare.rt_duo_ms, 0.0);
        // 已删除的参数（如响应阈值 gate_db）留在旧配置里也照样能读，直接忽略
        let old = parse("(params: (seed: 4, gate_db: -50.0))").unwrap();
        assert_eq!(old.params.seed, 4);
        assert!(matches!(parse("{\"other\": \"()\"}"), Err(ConfigError::NotAutoAsmr)));
        assert!(matches!(parse("not ron"), Err(ConfigError::Parse(_))));
    }
}
