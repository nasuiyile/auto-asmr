// Rust 层（auto-asmr-android/rust/src/lib.rs）导出的 C 接口。
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct Processor AsmrProcessor;
typedef struct Tele AsmrTele;

// 数据文件（ku100.bin、pinna.bin 的内容，从 APK 资源读出），在 asmr_init 之前提供一次
int asmr_provide_data(const uint8_t *ku100, size_t ku100_len, const uint8_t *pinna, size_t pinna_len);
int asmr_data_ready(void);
// 全局：加载数据表、生成默认配置（后台线程调用一次）
int asmr_init(void);
// 参数（RON 格式的 Params）；返回 0 = 成功
int asmr_set_params_ron(const char *ron);
int asmr_get_params_ron(char *buf, size_t cap);
// 风格：0 = 贴耳加强，1 = KU100 实录，2 = 掏耳，3 = 呼吸音，4 = 听歌
int asmr_set_style(int style);
int asmr_last_error(char *buf, size_t cap);
// 按钮：style_ear / style_ku100 / style_earclean / style_breath / style_music / reset / bg_rain_night / bg_off
int asmr_action(const char *name);
// 改一个参数（字段名或虚拟字段，值是 JSON）；读全部参数（JSON）
int asmr_set_field(const char *key, const char *value_json);
int asmr_get_params_json(char *buf, size_t cap);
// 输出效果 / 监听原声（存进配置文件，和桌面版共用）。原声时 asmr_proc_process 不进引擎，只乘输出增益、过峰值限制器
void asmr_set_dry(int dry);
// 配置文件（和桌面版同一格式）：导入整份换掉；导出返回长度，出错 -1
int asmr_load_config(const char *text);
// 只检查不导入：0 = 是 auto-asmr 的配置（buf = 概要），1 = 不是，2 = 读不懂（buf = 原因）
int asmr_check_config(const char *text, char *buf, size_t cap);
int asmr_export_config(char *buf, size_t cap);
// 界面用的参数表（JSON）
int asmr_ui_spec(char *buf, size_t cap);
// 引擎此刻的位置 → 手动模式的方位角、离头中心距离（米）、高度（度）
void asmr_manual_from_pose(float az, float el, float r, float *out3);

// 每个流一个处理器：在打开流的线程里建，关流之后释放
AsmrProcessor *asmr_proc_new(int sample_rate, int channels);
void asmr_proc_free(AsmrProcessor *p);
// 音频回调里调用：原地处理交错立体声 float；返回 -1 表示出过错，之后一律直通
int asmr_proc_process(AsmrProcessor *p, float *data, int frames);
float asmr_proc_latency_ms(const AsmrProcessor *p);
const AsmrTele *asmr_proc_tele(const AsmrProcessor *p);
// out[0..6]：方位角（度）、仰角（度）、距离（米）、说话活动、断流次数、是否出过错；
// out[6..11]：是不是左右双人（0 / 1），2 号声音的方位角、仰角、距离、说话活动（左右双人时前 4 项是 1 号）
void asmr_tele_read(const AsmrTele *t, float *out);

// 全局模式的输出节拍（每路一个）：按输出流里的积压量微调速度，卡顿后追回实时，见 rust/src/pace.rs
typedef struct Pacer AsmrPacer;
AsmrPacer *asmr_pace_new(void);
void asmr_pace_free(AsmrPacer *p);
// 输出流（重新）开始放：第一块是 first 帧时前面要先垫多少帧静音
int asmr_pace_begin(AsmrPacer *p, int first);
int asmr_pace_start_fill(const AsmrPacer *p);
// 送进 frames 帧处理好的交错立体声（不超过 4096 帧）；fill = 写之前输出流里还没放出去的帧数，underruns = 输出流累计断流次数。
// 要写进输出流的帧写到 out（可写 out_cap 帧，至少 frames + 64；再多 4800 帧，断流后才垫得下静音），返回帧数；0 = 这一块丢掉
int asmr_pace_push(AsmrPacer *p, const float *data, int frames, int fill, int underruns, float *out, int out_cap);
// out[0..4]：目标（写之前至少剩多少，毫秒）、速度偏差（ppm）、丢掉旧声音的次数、这一层的延时（毫秒）
void asmr_pace_stats(const AsmrPacer *p, float *out);

// 背景雨声（单独的输出流，48 kHz 立体声）
void *asmr_rain_new(void);
void asmr_rain_free(void *p);
// 返回 1 = 还有声音，0 = 没声音了（可以停流），-1 = 出错（写入静音）
int asmr_rain_render(void *p, float *data, int frames);
void asmr_rain_set_on(void *p, int on);

// 文件转换页：一条命令（JSON，UTF-8）进，整页状态（JSON，UTF-8）出；返回的字节用 asmr_free_bytes 释放
uint8_t *asmr_files(const uint8_t *cmd, size_t len, size_t *out_len);
void asmr_free_bytes(uint8_t *p, size_t len);

#ifdef __cplusplus
}
#endif
