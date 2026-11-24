// 截住 WebView（Chromium）打开的 AAudio 输出流，在它的数据回调之后原地处理声音。
//
// 做法（见 doc/android-plan.md）：用 ByteHook 做 PLT hook，换掉 libaaudio.so 的
//   AAudioStreamBuilder_setDataCallback：把 Chromium 的 (回调, userData) 换成 (data_cb, 上下文)；
//   AAudioStreamBuilder_openStream：记下流的格式，给立体声 float 流建一个 Rust 处理器（asmr_core.h）；
//   AAudioStream_close：关流之后释放处理器、上下文还回池子。
// data_cb 先调 Chromium 原来的回调让它填好这一块，再按模式处理：ASMR（Rust 引擎）/ 直通 / 左右互换 / 静音。
// 另有一个上报线程每秒打一行日志（处理前后的电平、处理时间、引擎状态），以及调试用的录音（处理前后各存一个 WAV）。
//
// 回调线程里不分配内存、不加锁、不打日志；上下文是固定大小的池子。

#include <aaudio/AAudio.h>
#include <android/log.h>
#include <dlfcn.h>
#include <jni.h>
#include <math.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#include "asmr_core.h"
#include "bytehook.h"
#include "shadowhook.h"

#define TAG "AsmrHook"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)
#define LOGW(...) __android_log_print(ANDROID_LOG_WARN, TAG, __VA_ARGS__)

enum { MODE_BYPASS = 0, MODE_ASMR = 1, MODE_SWAP = 2, MODE_MUTE = 3 };
static const char *const MODE_NAME[] = {"直通", "ASMR", "左右互换", "静音"};
static _Atomic int g_mode = MODE_ASMR;

enum { SLOT_FREE = 0, SLOT_PENDING = 1, SLOT_ACTIVE = 2, SLOT_CLOSED = 3 };
#define MAX_CTX 32

typedef struct {
    _Atomic int state;
    int id;
    AAudioStreamBuilder *builder;
    AAudioStream *stream;
    AAudioStream_dataCallback orig_cb;
    void *orig_user;
    char caller[96];
    int32_t rate, ch, fmt, perf, fpc, burst, usage;
    AsmrProcessor *proc; // 只处理立体声 float；其它格式为空，直通
    const AsmrTele *tele;
    float latency_ms;
    // 只有音频线程写，上报线程读（单写者，relaxed 即可）
    _Atomic uint64_t callbacks, frames, processed;
    _Atomic double e_in_l, e_in_r, e_out_l, e_out_r; // 累计能量（样本平方和）
    _Atomic uint64_t e_frames;
    _Atomic uint64_t proc_ns, proc_frames; // ASMR 处理累计用时和帧数
    _Atomic uint32_t last_n, max_proc_ns;
    _Atomic uint64_t last_cb_ns; // 最近一次回调的时刻（界面取"正在出声的流"用）
    uint64_t u_proc_ns, u_proc_frames; // 界面读处理占用的上一次快照
    // 上报线程自己的上一次快照
    uint64_t r_callbacks, r_e_frames, r_proc_ns, r_proc_frames;
    double r_in_l, r_in_r, r_out_l, r_out_r;
} ctx_t;

static ctx_t g_ctx[MAX_CTX];
static _Atomic int g_next_id = 1;
static char g_hook_log[1024];
static pthread_mutex_t g_hook_log_mu = PTHREAD_MUTEX_INITIALIZER;
// 上报线程读流的状态、关流时释放处理器，都在这把锁下（音频回调不碰它）
static pthread_mutex_t g_report_mu = PTHREAD_MUTEX_INITIALIZER;

#define LD(x) atomic_load_explicit(&(x), memory_order_relaxed)
#define ST(x, v) atomic_store_explicit(&(x), (v), memory_order_relaxed)

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static const char *base_name(const char *p) {
    const char *s = p ? strrchr(p, '/') : NULL;
    return s ? s + 1 : (p ? p : "?");
}

static const char *perf_name(int32_t p) {
    switch (p) {
    case AAUDIO_PERFORMANCE_MODE_NONE: return "NONE";
    case AAUDIO_PERFORMANCE_MODE_POWER_SAVING: return "POWER_SAVING";
    case AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: return "LOW_LATENCY";
    default: return "?";
    }
}

static const char *fmt_name(int32_t f) {
    switch (f) {
    case AAUDIO_FORMAT_PCM_FLOAT: return "float";
    case AAUDIO_FORMAT_PCM_I16: return "i16";
    default: return "other";
    }
}

// ---------------------------------------------------------------- 调试录音：处理前后各录一份

enum { REC_IDLE = 0, REC_ARMED = 1, REC_RUNNING = 2, REC_DONE = 3 };
static struct {
    _Atomic int state;
    _Atomic int owner; // 流 id
    _Atomic int rate;
    float *dry, *wet; // 交错立体声
    size_t cap;       // 帧
    int seconds;
    _Atomic size_t n; // 已录帧数
    char dir[512];
} g_rec;

static void rec_before(ctx_t *c, const float *x, int32_t n, size_t *pos, size_t *cnt) {
    *cnt = 0;
    int s = atomic_load(&g_rec.state);
    if (s == REC_ARMED) {
        int expect = REC_ARMED;
        if (atomic_compare_exchange_strong(&g_rec.state, &expect, REC_RUNNING)) {
            atomic_store(&g_rec.owner, c->id);
            atomic_store(&g_rec.rate, c->rate);
            s = REC_RUNNING;
        }
    }
    if (s != REC_RUNNING || atomic_load(&g_rec.owner) != c->id) return;
    size_t want = (size_t)g_rec.seconds * (size_t)c->rate;
    if (want > g_rec.cap) want = g_rec.cap;
    size_t at = atomic_load(&g_rec.n);
    if (at >= want) return;
    size_t m = (size_t)n < want - at ? (size_t)n : want - at;
    memcpy(g_rec.dry + at * 2, x, m * 2 * sizeof(float));
    *pos = at;
    *cnt = m;
}

static void rec_after(const float *x, size_t pos, size_t cnt, int rate) {
    if (cnt == 0) return;
    memcpy(g_rec.wet + pos * 2, x, cnt * 2 * sizeof(float));
    atomic_store(&g_rec.n, pos + cnt);
    size_t want = (size_t)g_rec.seconds * (size_t)rate;
    if (want > g_rec.cap) want = g_rec.cap;
    if (pos + cnt >= want) atomic_store(&g_rec.state, REC_DONE);
}

static int write_wav24(const char *path, const float *x, size_t frames, int rate) {
    FILE *f = fopen(path, "wb");
    if (!f) return -1;
    uint32_t data = (uint32_t)(frames * 6);
    uint8_t h[44];
    memcpy(h, "RIFF", 4);
    uint32_t v = 36 + data;
    memcpy(h + 4, &v, 4);
    memcpy(h + 8, "WAVEfmt ", 8);
    v = 16;
    memcpy(h + 16, &v, 4);
    uint16_t s = 1;
    memcpy(h + 20, &s, 2); // PCM
    s = 2;
    memcpy(h + 22, &s, 2); // 声道
    v = (uint32_t)rate;
    memcpy(h + 24, &v, 4);
    v = (uint32_t)rate * 6;
    memcpy(h + 28, &v, 4);
    s = 6;
    memcpy(h + 32, &s, 2);
    s = 24;
    memcpy(h + 34, &s, 2);
    memcpy(h + 36, "data", 4);
    memcpy(h + 40, &data, 4);
    fwrite(h, 1, 44, f);
    uint8_t buf[6 * 1024];
    size_t k = 0;
    for (size_t i = 0; i < frames * 2; i++) {
        float y = x[i];
        if (!(y > -1.0f)) y = -1.0f; // 也挡住 NaN
        if (y > 1.0f) y = 1.0f;
        int32_t q = (int32_t)lrintf(y * 8388607.0f);
        buf[k++] = (uint8_t)(q & 0xff);
        buf[k++] = (uint8_t)((q >> 8) & 0xff);
        buf[k++] = (uint8_t)((q >> 16) & 0xff);
        if (k == sizeof buf) {
            fwrite(buf, 1, k, f);
            k = 0;
        }
    }
    if (k) fwrite(buf, 1, k, f);
    return fclose(f);
}

static void *rec_writer(void *arg) {
    (void)arg;
    // 最多等 录音秒数 + 60 秒（要等网页开始出声）
    for (int i = 0; i < (g_rec.seconds + 60) * 10 && atomic_load(&g_rec.state) != REC_DONE; i++) usleep(100000);
    // 没录满就超时：先让音频线程停手，等一个回调的时间再读
    atomic_store(&g_rec.state, REC_DONE);
    usleep(300000);
    size_t n = atomic_load(&g_rec.n);
    int rate = atomic_load(&g_rec.rate);
    if (n > 0 && rate > 0) {
        char a[640], b[640];
        long t = (long)time(NULL);
        snprintf(a, sizeof a, "%s/rec_%ld_dry.wav", g_rec.dir, t);
        snprintf(b, sizeof b, "%s/rec_%ld_wet.wav", g_rec.dir, t);
        int ra = write_wav24(a, g_rec.dry, n, rate), rb = write_wav24(b, g_rec.wet, n, rate);
        LOGI("录音完成：流#%d %d Hz %.1f 秒 → %s / %s（%s）", atomic_load(&g_rec.owner), rate, (double)n / rate, a, b,
             ra == 0 && rb == 0 ? "已保存" : "写文件失败");
    } else {
        LOGW("录音：没有等到出声的流");
    }
    free(g_rec.dry);
    free(g_rec.wet);
    g_rec.dry = g_rec.wet = NULL;
    atomic_store(&g_rec.state, REC_IDLE);
    return NULL;
}

// ---------------------------------------------------------------- 数据回调

static void energy(const float *x, int32_t n, double *l, double *r) {
    double a = 0, b = 0;
    for (int32_t i = 0; i < n; i++) {
        a += (double)x[2 * i] * x[2 * i];
        b += (double)x[2 * i + 1] * x[2 * i + 1];
    }
    *l = a;
    *r = b;
}

static aaudio_data_callback_result_t data_cb(AAudioStream *stream, void *user, void *audio, int32_t n) {
    ctx_t *c = (ctx_t *)user;
    aaudio_data_callback_result_t r = c->orig_cb(stream, c->orig_user, audio, n);
    ST(c->callbacks, LD(c->callbacks) + 1);
    ST(c->frames, LD(c->frames) + (uint64_t)n);
    ST(c->last_cb_ns, now_ns());
    ST(c->last_n, (uint32_t)n);
    if (c->fmt != AAUDIO_FORMAT_PCM_FLOAT || c->ch != 2 || !audio || n <= 0) return r;

    float *x = (float *)audio;
    double il, ir, ol, or_;
    energy(x, n, &il, &ir);
    size_t rpos = 0, rcnt = 0;
    rec_before(c, x, n, &rpos, &rcnt);

    int mode = atomic_load_explicit(&g_mode, memory_order_relaxed);
    switch (mode) {
    case MODE_ASMR:
        if (c->proc) {
            uint64_t t0 = now_ns();
            asmr_proc_process(c->proc, x, n); // 出过错时返回 -1、数据不动（直通）
            uint32_t dt = (uint32_t)(now_ns() - t0);
            ST(c->proc_ns, LD(c->proc_ns) + dt);
            ST(c->proc_frames, LD(c->proc_frames) + (uint64_t)n);
            if (dt > LD(c->max_proc_ns)) ST(c->max_proc_ns, dt);
        }
        break;
    case MODE_SWAP:
        for (int32_t i = 0; i < n; i++) {
            float t = x[2 * i];
            x[2 * i] = x[2 * i + 1];
            x[2 * i + 1] = t;
        }
        break;
    case MODE_MUTE:
        memset(x, 0, (size_t)n * 2 * sizeof(float));
        break;
    default:
        break;
    }

    rec_after(x, rpos, rcnt, c->rate);
    energy(x, n, &ol, &or_);
    ST(c->e_in_l, LD(c->e_in_l) + il);
    ST(c->e_in_r, LD(c->e_in_r) + ir);
    ST(c->e_out_l, LD(c->e_out_l) + ol);
    ST(c->e_out_r, LD(c->e_out_r) + or_);
    ST(c->e_frames, LD(c->e_frames) + (uint64_t)n);
    ST(c->processed, LD(c->processed) + (uint64_t)n);
    return r;
}

// ---------------------------------------------------------------- 池子

static ctx_t *ctx_alloc(void) {
    for (int i = 0; i < MAX_CTX; i++) {
        int expect = SLOT_FREE;
        if (atomic_compare_exchange_strong(&g_ctx[i].state, &expect, SLOT_PENDING)) return &g_ctx[i];
    }
    return NULL;
}

static void ctx_reset(ctx_t *c) {
    c->builder = NULL;
    c->stream = NULL;
    c->proc = NULL;
    c->tele = NULL;
    c->latency_ms = 0;
    ST(c->callbacks, 0);
    ST(c->frames, 0);
    ST(c->processed, 0);
    ST(c->e_in_l, 0);
    ST(c->e_in_r, 0);
    ST(c->e_out_l, 0);
    ST(c->e_out_r, 0);
    ST(c->e_frames, 0);
    ST(c->proc_ns, 0);
    ST(c->proc_frames, 0);
    ST(c->last_n, 0);
    ST(c->max_proc_ns, 0);
    ST(c->last_cb_ns, 0);
    c->u_proc_ns = c->u_proc_frames = 0;
    c->r_callbacks = c->r_e_frames = c->r_proc_ns = c->r_proc_frames = 0;
    c->r_in_l = c->r_in_r = c->r_out_l = c->r_out_r = 0;
}

static ctx_t *ctx_find(int state, const void *builder, const void *stream) {
    for (int i = 0; i < MAX_CTX; i++) {
        ctx_t *c = &g_ctx[i];
        if (atomic_load(&c->state) != state) continue;
        if (builder && c->builder == builder) return c;
        if (stream && c->stream == stream) return c;
    }
    return NULL;
}

// ---------------------------------------------------------------- 代理函数
//
// 两种 hook 共用同一套逻辑（do_*），只是"原函数"从哪来不同：
//   PLT hook（ByteHook，默认）：只截 WebView 等调用方 so 对 libaaudio 的调用，原函数由 ByteHook 给；
//   inline hook（ShadowHook，备用）：直接改 libaaudio 里的函数本身，谁调都截，原函数存在 g_orig_*。
// 两种同时开着时，一次调用会先后经过两个代理：用线程局部的 g_in_proxy 防重入，只处理一次。
// App 自己开的流（背景雨声）不处理。

typedef void (*set_cb_t)(AAudioStreamBuilder *, AAudioStream_dataCallback, void *);
typedef aaudio_result_t (*open_t)(AAudioStreamBuilder *, AAudioStream **);
typedef aaudio_result_t (*close_t)(AAudioStream *);

static __thread int g_in_proxy;

/// 调用方是不是本 so（自己开的雨声流不处理）。
static bool from_self(void *ra) {
    Dl_info info;
    return ra && dladdr(ra, &info) && info.dli_fname && strstr(info.dli_fname, "libasmrhook");
}

static void do_set_cb(set_cb_t prev, AAudioStreamBuilder *b, AAudioStream_dataCallback cb, void *user, void *ra) {
    ctx_t *c = NULL;
    if (cb && cb != data_cb) {
        // 同一个 builder 再设一次：先把旧的放掉
        ctx_t *old = ctx_find(SLOT_PENDING, b, NULL);
        if (old) atomic_store(&old->state, SLOT_FREE);
        c = ctx_alloc();
    }
    if (c) {
        ctx_reset(c);
        c->id = atomic_fetch_add(&g_next_id, 1);
        c->builder = b;
        c->orig_cb = cb;
        c->orig_user = user;
        Dl_info info;
        snprintf(c->caller, sizeof c->caller, "%s", (ra && dladdr(ra, &info) && info.dli_fname) ? base_name(info.dli_fname) : "?");
        prev(b, data_cb, c);
    } else {
        if (cb && cb != data_cb) LOGW("上下文池满了，这个流不处理");
        prev(b, cb, user);
    }
}

static aaudio_result_t do_open(open_t prev, AAudioStreamBuilder *b, AAudioStream **s) {
    aaudio_result_t r = prev(b, s);
    ctx_t *c = ctx_find(SLOT_PENDING, b, NULL);
    if (c) {
        if (r == AAUDIO_OK && s && *s) {
            AAudioStream *st = *s;
            c->stream = st;
            c->builder = NULL;
            c->rate = AAudioStream_getSampleRate(st);
            c->ch = AAudioStream_getChannelCount(st);
            c->fmt = AAudioStream_getFormat(st);
            c->perf = AAudioStream_getPerformanceMode(st);
            c->fpc = AAudioStream_getFramesPerDataCallback(st);
            c->burst = AAudioStream_getFramesPerBurst(st);
            c->usage = AAudioStream_getUsage(st);
            // 回调要等 requestStart 之后才来，这里建处理器来得及（不在音频线程里）
            if (c->fmt == AAUDIO_FORMAT_PCM_FLOAT && c->ch == 2) {
                uint64_t t0 = now_ns();
                c->proc = asmr_proc_new(c->rate, c->ch);
                if (c->proc) {
                    c->tele = asmr_proc_tele(c->proc);
                    c->latency_ms = asmr_proc_latency_ms(c->proc);
                }
                LOGI("流#%d 建处理器 %s，用时 %.1f ms，处理延迟 %.1f ms", c->id,
                     c->proc ? "成功" : (asmr_data_ready() ? "失败" : "跳过（数据还没加载）"), (double)(now_ns() - t0) / 1e6,
                     c->latency_ms);
            }
            atomic_store(&c->state, SLOT_ACTIVE);
            LOGI("流#%d 打开 [%s] %d Hz %d 声道 %s 性能模式=%s 每次回调=%d 帧 burst=%d usage=%d", c->id, c->caller, c->rate, c->ch,
                 fmt_name(c->fmt), perf_name(c->perf), c->fpc, c->burst, c->usage);
        } else {
            LOGW("流#%d 打开失败 %d", c->id, r);
            atomic_store(&c->state, SLOT_FREE);
        }
    }
    return r;
}

static aaudio_result_t do_close(close_t prev, AAudioStream *s) {
    pthread_mutex_lock(&g_report_mu);
    ctx_t *c = ctx_find(SLOT_ACTIVE, NULL, s);
    if (c) atomic_store(&c->state, SLOT_CLOSED); // 上报线程不再碰它
    pthread_mutex_unlock(&g_report_mu);
    if (c && atomic_load(&g_rec.owner) == c->id) {
        int expect = REC_RUNNING;
        atomic_compare_exchange_strong(&g_rec.state, &expect, REC_DONE); // 正在录的流关了：录到哪算哪
    }
    aaudio_result_t r = prev(s);
    if (c) {
        // close 返回后不会再有回调，可以释放处理器
        uint64_t pf = LD(c->proc_frames);
        LOGI("流#%d 关闭：回调 %llu 次，共 %llu 帧，其中 ASMR 处理 %llu 帧，平均每 1000 帧 %.0f µs，单次回调最长 %u µs", c->id,
             (unsigned long long)LD(c->callbacks), (unsigned long long)LD(c->frames), (unsigned long long)pf,
             pf ? (double)LD(c->proc_ns) / (double)pf : 0.0, LD(c->max_proc_ns) / 1000);
        asmr_proc_free(c->proc);
        c->proc = NULL;
        c->tele = NULL;
        atomic_store(&c->state, SLOT_FREE);
    }
    return r;
}

// ---- PLT hook（ByteHook）的代理
static void plt_set_data_callback(AAudioStreamBuilder *b, AAudioStream_dataCallback cb, void *user) {
    set_cb_t prev = (set_cb_t)bytehook_get_prev_func((void *)plt_set_data_callback);
    if (g_in_proxy) {
        prev(b, cb, user);
    } else {
        g_in_proxy = 1;
        do_set_cb(prev, b, cb, user, BYTEHOOK_RETURN_ADDRESS());
        g_in_proxy = 0;
    }
    BYTEHOOK_POP_STACK();
}

static aaudio_result_t plt_open_stream(AAudioStreamBuilder *b, AAudioStream **s) {
    open_t prev = (open_t)bytehook_get_prev_func((void *)plt_open_stream);
    aaudio_result_t r;
    if (g_in_proxy) {
        r = prev(b, s);
    } else {
        g_in_proxy = 1;
        r = do_open(prev, b, s);
        g_in_proxy = 0;
    }
    BYTEHOOK_POP_STACK();
    return r;
}

static aaudio_result_t plt_close(AAudioStream *s) {
    close_t prev = (close_t)bytehook_get_prev_func((void *)plt_close);
    aaudio_result_t r;
    if (g_in_proxy) {
        r = prev(s);
    } else {
        g_in_proxy = 1;
        r = do_close(prev, s);
        g_in_proxy = 0;
    }
    BYTEHOOK_POP_STACK();
    return r;
}

// ---- inline hook（ShadowHook，独占模式：原函数直接调）的代理
static set_cb_t g_orig_set_cb;
static open_t g_orig_open;
static close_t g_orig_close;

static void inl_set_data_callback(AAudioStreamBuilder *b, AAudioStream_dataCallback cb, void *user) {
    void *ra = __builtin_return_address(0);
    if (g_in_proxy || from_self(ra)) {
        g_orig_set_cb(b, cb, user);
        return;
    }
    g_in_proxy = 1;
    do_set_cb(g_orig_set_cb, b, cb, user, ra);
    g_in_proxy = 0;
}

static aaudio_result_t inl_open_stream(AAudioStreamBuilder *b, AAudioStream **s) {
    if (g_in_proxy || from_self(__builtin_return_address(0))) return g_orig_open(b, s);
    g_in_proxy = 1;
    aaudio_result_t r = do_open(g_orig_open, b, s);
    g_in_proxy = 0;
    return r;
}

static aaudio_result_t inl_close(AAudioStream *s) {
    if (g_in_proxy || from_self(__builtin_return_address(0))) return g_orig_close(s);
    g_in_proxy = 1;
    aaudio_result_t r = do_close(g_orig_close, s);
    g_in_proxy = 0;
    return r;
}

// ---------------------------------------------------------------- 背景雨声：自己的一条输出流（48 kHz 立体声）
//
// 打开（setRain(true)）时开流；关掉时让 Rust 淡出，淡完回调返回 STOP，上报线程再把流关掉、释放渲染器。
// 耳机拔插等导致流断开时（错误回调），上报线程按需要重开。

static struct {
    pthread_mutex_t mu;
    AAudioStream *stream;
    void *rp; // RainOut*
    _Atomic int want;
    _Atomic int dead;
    _Atomic int playing;
} g_rain = {PTHREAD_MUTEX_INITIALIZER, NULL, NULL, 0, 0, 0};

static aaudio_data_callback_result_t rain_cb(AAudioStream *s, void *user, void *audio, int32_t n) {
    (void)s;
    int r = asmr_rain_render(user, (float *)audio, n);
    atomic_store(&g_rain.playing, r == 1);
    if (r == 0 && !atomic_load(&g_rain.want)) return AAUDIO_CALLBACK_RESULT_STOP;
    return AAUDIO_CALLBACK_RESULT_CONTINUE;
}

static void rain_err_cb(AAudioStream *s, void *user, aaudio_result_t err) {
    (void)s;
    (void)user;
    LOGW("雨声流出错 %d（%s），稍后重开", err, AAudio_convertResultToText(err));
    atomic_store(&g_rain.dead, 1);
}

// 以下两个在 g_rain.mu 里调用
static void rain_close_locked(void) {
    if (g_rain.stream) {
        AAudioStream_requestStop(g_rain.stream);
        AAudioStream_close(g_rain.stream);
        g_rain.stream = NULL;
    }
    if (g_rain.rp) {
        asmr_rain_free(g_rain.rp);
        g_rain.rp = NULL;
    }
    atomic_store(&g_rain.playing, 0);
}

static void rain_open_locked(void) {
    rain_close_locked();
    atomic_store(&g_rain.dead, 0);
    g_rain.rp = asmr_rain_new();
    if (!g_rain.rp) {
        LOGW("雨声：数据还没加载，稍后再开");
        atomic_store(&g_rain.dead, 1);
        return;
    }
    AAudioStreamBuilder *b = NULL;
    if (AAudio_createStreamBuilder(&b) != AAUDIO_OK) return;
    AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_FLOAT);
    AAudioStreamBuilder_setChannelCount(b, 2);
    AAudioStreamBuilder_setSampleRate(b, 48000);
    AAudioStreamBuilder_setUsage(b, AAUDIO_USAGE_MEDIA);
    AAudioStreamBuilder_setContentType(b, AAUDIO_CONTENT_TYPE_MUSIC);
    AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_POWER_SAVING);
    AAudioStreamBuilder_setDataCallback(b, rain_cb, g_rain.rp);
    AAudioStreamBuilder_setErrorCallback(b, rain_err_cb, NULL);
    aaudio_result_t r = AAudioStreamBuilder_openStream(b, &g_rain.stream);
    AAudioStreamBuilder_delete(b);
    if (r != AAUDIO_OK || !g_rain.stream) {
        LOGW("雨声流打开失败：%s", AAudio_convertResultToText(r));
        g_rain.stream = NULL;
        atomic_store(&g_rain.dead, 1);
        return;
    }
    r = AAudioStream_requestStart(g_rain.stream);
    LOGI("雨声流打开：%d Hz，%s", AAudioStream_getSampleRate(g_rain.stream), r == AAUDIO_OK ? "开始播放" : AAudio_convertResultToText(r));
}

static void rain_set(bool on) {
    pthread_mutex_lock(&g_rain.mu);
    atomic_store(&g_rain.want, on);
    if (on) {
        aaudio_stream_state_t st = g_rain.stream ? AAudioStream_getState(g_rain.stream) : AAUDIO_STREAM_STATE_UNINITIALIZED;
        if (!g_rain.stream || atomic_load(&g_rain.dead) || st == AAUDIO_STREAM_STATE_STOPPED || st == AAUDIO_STREAM_STATE_STOPPING) {
            rain_open_locked();
        } else {
            asmr_rain_set_on(g_rain.rp, 1); // 淡出途中又打开
        }
    } else if (g_rain.rp) {
        asmr_rain_set_on(g_rain.rp, 0); // 淡出，淡完回调返回 STOP
    }
    pthread_mutex_unlock(&g_rain.mu);
}

/// 上报线程每秒调一次：停下的流关掉；断开的流按需要重开。
static void rain_housekeeping(void) {
    pthread_mutex_lock(&g_rain.mu);
    bool want = atomic_load(&g_rain.want);
    if (g_rain.stream) {
        aaudio_stream_state_t st = AAudioStream_getState(g_rain.stream);
        bool stopped = st == AAUDIO_STREAM_STATE_STOPPED || st == AAUDIO_STREAM_STATE_DISCONNECTED;
        if (atomic_load(&g_rain.dead) || (!want && stopped)) {
            LOGI("雨声流关闭%s", want ? "（断开了，重开）" : "");
            rain_close_locked();
        }
    }
    if (want && (!g_rain.stream || atomic_load(&g_rain.dead))) rain_open_locked();
    pthread_mutex_unlock(&g_rain.mu);
}

// ---------------------------------------------------------------- 上报

static double db(double e, uint64_t n) {
    if (n == 0) return -999.0;
    double ms = e / (double)n;
    return ms > 1e-12 ? 10.0 * log10(ms) : -120.0;
}

// for_log：给日志用（只报这 1 秒里有回调的流，并推进快照）；否则给界面用（不推进快照）
static int format_stats(char *out, size_t cap, bool for_log) {
    int len = 0, active = 0;
    pthread_mutex_lock(&g_report_mu);
    for (int i = 0; i < MAX_CTX; i++) {
        ctx_t *c = &g_ctx[i];
        if (atomic_load(&c->state) != SLOT_ACTIVE) continue;
        active++;
        uint64_t cb = LD(c->callbacks), ef = LD(c->e_frames), pn = LD(c->proc_ns), pf = LD(c->proc_frames);
        double il = LD(c->e_in_l), ir = LD(c->e_in_r), ol = LD(c->e_out_l), orr = LD(c->e_out_r);
        uint64_t dcb = cb - c->r_callbacks, dn = ef - c->r_e_frames, dpn = pn - c->r_proc_ns, dpf = pf - c->r_proc_frames;
        if (for_log && dcb == 0) continue; // 暂停中的流不刷屏
        int w = snprintf(out + len, cap - (size_t)len,
                         "流#%d %dHz %dch %s | 回调 %llu 次/秒 每次 %u 帧 | 处理前 L %.1f R %.1f dB → 处理后 L %.1f R %.1f dB", c->id,
                         c->rate, c->ch, fmt_name(c->fmt), (unsigned long long)dcb, LD(c->last_n), db(il - c->r_in_l, dn),
                         db(ir - c->r_in_r, dn), db(ol - c->r_out_l, dn), db(orr - c->r_out_r, dn));
        if (w < 0 || (size_t)(len + w) >= cap) break;
        len += w;
        if (c->tele) {
            float t[11];
            asmr_tele_read(c->tele, t);
            // 处理占用 = 处理用时 / 这些帧的播放时长
            double load = dpf ? (double)dpn / ((double)dpf / c->rate * 1e9) * 100.0 : 0.0;
            w = snprintf(out + len, cap - (size_t)len,
                         " | 引擎：方位 %.0f° 仰角 %.0f° 距离 %.1f cm 说话 %.2f | 处理 %.2f%% 单核 最长 %u µs | 延迟 %.1f ms | 断流 %.0f xrun %d%s",
                         t[0], t[1], t[2] * 100.0f, t[3], load, LD(c->max_proc_ns) / 1000, c->latency_ms, t[4],
                         AAudioStream_getXRunCount(c->stream), t[5] > 0 ? " | 引擎出过错，已改直通" : "");
            if (w < 0 || (size_t)(len + w) >= cap) break;
            len += w;
        }
        if ((size_t)len + 2 > cap) break;
        out[len++] = '\n';
        out[len] = 0;
        if (for_log) {
            c->r_callbacks = cb;
            c->r_e_frames = ef;
            c->r_proc_ns = pn;
            c->r_proc_frames = pf;
            c->r_in_l = il;
            c->r_in_r = ir;
            c->r_out_l = ol;
            c->r_out_r = orr;
        }
    }
    pthread_mutex_unlock(&g_report_mu);
    if (!for_log && active == 0) len = snprintf(out, cap, "没有打开的 AAudio 流\n");
    return len;
}

static void *reporter(void *arg) {
    (void)arg;
    char buf[8192];
    for (;;) {
        sleep(1);
        rain_housekeeping();
        if (format_stats(buf, sizeof buf, true) > 0) {
            char *save = NULL;
            for (char *line = strtok_r(buf, "\n", &save); line; line = strtok_r(NULL, "\n", &save))
                LOGI("%s | 模式=%s", line, MODE_NAME[atomic_load(&g_mode)]);
        }
    }
    return NULL;
}

// ---------------------------------------------------------------- 安装

static void hooked(bytehook_stub_t stub, int status, const char *caller, const char *sym, void *new_func, void *prev_func,
                   void *arg) {
    (void)stub;
    (void)new_func;
    (void)prev_func;
    (void)arg;
    LOGI("hook %s @ %s：%s(%d)", sym, base_name(caller), status == BYTEHOOK_STATUS_CODE_OK ? "成功" : "失败", status);
    if (status == BYTEHOOK_STATUS_CODE_OK && strcmp(sym, "AAudioStreamBuilder_setDataCallback") == 0) {
        pthread_mutex_lock(&g_hook_log_mu);
        size_t l = strlen(g_hook_log);
        snprintf(g_hook_log + l, sizeof g_hook_log - l, "%s%s", l ? "、" : "", base_name(caller));
        pthread_mutex_unlock(&g_hook_log_mu);
    }
}

static bool allow_caller(const char *caller, void *arg) {
    (void)arg;
    // 自己和 ByteHook 不 hook；其余凡是调用这几个函数的 so 都 hook（WebView 的 so 名字因版本 / 厂商而异）
    return strstr(caller, "libasmrhook") == NULL && strstr(caller, "libbytehook") == NULL;
}

static int g_plt_installed = 0, g_inline_installed = 0;
static _Atomic int g_inline_hooked = 0; // inline hook 装上了几个函数

static void sh_hooked(int err, const char *lib, const char *sym, void *sym_addr, void *new_addr, void *orig_addr, void *arg) {
    (void)lib;
    (void)sym_addr;
    (void)new_addr;
    (void)orig_addr;
    (void)arg;
    LOGI("inline hook %s：%s(%d %s)", sym, err == 0 ? "成功" : "失败", err, shadowhook_to_errmsg(err));
    if (err == 0) atomic_fetch_add(&g_inline_hooked, 1);
}

/// 备用：用 ShadowHook 直接改 libaaudio 里的函数（谁调都截）。PLT hook 截不到时用。返回失败的个数。
static int install_inline(void) {
    if (g_inline_installed) return 0;
    g_inline_installed = 1;
    int r = shadowhook_init(SHADOWHOOK_MODE_SHARED, false);
    if (r != 0) LOGW("shadowhook_init：%d %s", r, shadowhook_to_errmsg(r));
    static const char *const syms[] = {"AAudioStreamBuilder_setDataCallback", "AAudioStreamBuilder_openStream", "AAudioStream_close"};
    void *const funcs[] = {(void *)inl_set_data_callback, (void *)inl_open_stream, (void *)inl_close};
    void **const origs[] = {(void **)&g_orig_set_cb, (void **)&g_orig_open, (void **)&g_orig_close};
    int fails = 0;
    for (int i = 0; i < 3; i++) {
        if (!shadowhook_hook_sym_name_callback_2("libaaudio.so", syms[i], funcs[i], origs[i], SHADOWHOOK_HOOK_WITH_UNIQUE_MODE, sh_hooked, NULL)) {
            int e = shadowhook_get_errno();
            LOGW("inline hook %s 失败：%d %s", syms[i], e, shadowhook_to_errmsg(e));
            fails++;
        }
    }
    return fails;
}

static int install_plt(void) {
    if (g_plt_installed) return 0;
    g_plt_installed = 1;
    static const char *const syms[] = {"AAudioStreamBuilder_setDataCallback", "AAudioStreamBuilder_openStream", "AAudioStream_close"};
    void *const funcs[] = {(void *)plt_set_data_callback, (void *)plt_open_stream, (void *)plt_close};
    int fails = 0;
    for (int i = 0; i < 3; i++) {
        if (!bytehook_hook_partial(allow_caller, NULL, "libaaudio.so", syms[i], funcs[i], hooked, NULL)) {
            LOGW("bytehook_hook_partial(%s) 返回空", syms[i]);
            fails++;
        }
    }
    return fails;
}

#define JNI_FN(name) Java_io_github_nasuiyile_autoasmr_NativeHook_##name

static int g_started = 0;

/// 登记 hook。how：0 = PLT hook（默认），1 = 只用 inline hook（测试备用路径），2 = 两种都装，3 = 都不装（测自检）。
/// 返回失败的个数。
JNIEXPORT jint JNICALL JNI_FN(install)(JNIEnv *env, jclass cls, jint how) {
    (void)env;
    (void)cls;
    int fails = 0;
    if (how == 0 || how == 2) fails += install_plt();
    if (how == 1 || how == 2) fails += install_inline();
    if (!g_started) {
        g_started = 1;
        pthread_t t;
        pthread_create(&t, NULL, reporter, NULL);
        pthread_detach(t);
    }
    LOGI("hook 已登记（%s，%d 个失败），模式=%s", how == 0 ? "PLT" : how == 1 ? "inline" : how == 2 ? "PLT + inline" : "都不装（测试）", fails,
         MODE_NAME[atomic_load(&g_mode)]);
    return fails;
}

/// 自检发现截不到声音时调用：再装上 inline hook（之后新开的流就能截到）。返回失败的个数。
JNIEXPORT jint JNICALL JNI_FN(installInline)(JNIEnv *env, jclass cls) {
    (void)env;
    (void)cls;
    int already = g_inline_installed;
    int fails = install_inline();
    if (!already) LOGI("改用备用的 inline hook：%d 个失败", fails);
    return fails;
}

/// hook 的状态：[0] PLT hook 截到的调用方个数 [1] inline hook 装上的函数个数 [2] 最近 1 秒里还在回调的被截流个数（任何格式）。
JNIEXPORT jintArray JNICALL JNI_FN(hookState)(JNIEnv *env, jclass cls) {
    (void)cls;
    int out[3] = {0, 0, 0};
    pthread_mutex_lock(&g_hook_log_mu);
    if (g_hook_log[0]) {
        out[0] = 1;
        for (const char *p = g_hook_log; (p = strstr(p, "、")); p += strlen("、")) out[0]++;
    }
    pthread_mutex_unlock(&g_hook_log_mu);
    out[1] = atomic_load(&g_inline_hooked);
    uint64_t now = now_ns();
    for (int i = 0; i < MAX_CTX; i++) {
        ctx_t *c = &g_ctx[i];
        if (atomic_load(&c->state) == SLOT_ACTIVE && now - LD(c->last_cb_ns) < 1000000000ull) out[2]++;
    }
    jintArray a = (*env)->NewIntArray(env, 3);
    if (a) (*env)->SetIntArrayRegion(env, a, 0, 3, out);
    return a;
}

/// 提供数据文件（从 APK 资源读出来的 ku100.bin、pinna.bin）。返回 0 = 成功，1 = 已经提供过。
JNIEXPORT jint JNICALL JNI_FN(provideData)(JNIEnv *env, jclass cls, jbyteArray ku100, jbyteArray pinna) {
    (void)cls;
    jsize n1 = (*env)->GetArrayLength(env, ku100), n2 = (*env)->GetArrayLength(env, pinna);
    jbyte *a = (*env)->GetByteArrayElements(env, ku100, NULL);
    jbyte *b = (*env)->GetByteArrayElements(env, pinna, NULL);
    int r = (a && b) ? asmr_provide_data((const uint8_t *)a, (size_t)n1, (const uint8_t *)b, (size_t)n2) : -1;
    if (a) (*env)->ReleaseByteArrayElements(env, ku100, a, JNI_ABORT);
    if (b) (*env)->ReleaseByteArrayElements(env, pinna, b, JNI_ABORT);
    return r;
}

/// 背景雨声：打开（开输出流）/ 关掉（淡出后停）。
JNIEXPORT void JNICALL JNI_FN(setRain)(JNIEnv *env, jclass cls, jboolean on) {
    (void)env;
    (void)cls;
    rain_set(on);
}

/// 雨声此刻有没有在响。
JNIEXPORT jboolean JNICALL JNI_FN(rainPlaying)(JNIEnv *env, jclass cls) {
    (void)env;
    (void)cls;
    return atomic_load(&g_rain.playing) ? JNI_TRUE : JNI_FALSE;
}

JNIEXPORT jint JNICALL JNI_FN(initEngine)(JNIEnv *env, jclass cls) {
    (void)env;
    (void)cls;
    return asmr_init();
}

JNIEXPORT void JNICALL JNI_FN(setMode)(JNIEnv *env, jclass cls, jint mode) {
    (void)env;
    (void)cls;
    if (mode < MODE_BYPASS || mode > MODE_MUTE) return;
    atomic_store(&g_mode, mode);
    LOGI("模式 → %s", MODE_NAME[mode]);
}

JNIEXPORT jint JNICALL JNI_FN(setStyle)(JNIEnv *env, jclass cls, jint style) {
    (void)env;
    (void)cls;
    int r = asmr_set_style(style);
    LOGI("风格 → %d：%s", style, r == 0 ? "成功" : "失败");
    return r;
}

JNIEXPORT jint JNICALL JNI_FN(setParamsRon)(JNIEnv *env, jclass cls, jstring ron) {
    (void)cls;
    const char *s = (*env)->GetStringUTFChars(env, ron, NULL);
    if (!s) return -1;
    int r = asmr_set_params_ron(s);
    (*env)->ReleaseStringUTFChars(env, ron, s);
    return r;
}

static jstring take_string(JNIEnv *env, int (*get)(char *, size_t)) {
    int n = get(NULL, 0);
    if (n < 0) return NULL; // 出错：Kotlin 拿到 null，原因用 lastError() 取
    char *buf = malloc((size_t)n + 1);
    if (!buf) return (*env)->NewStringUTF(env, "");
    get(buf, (size_t)n + 1);
    jstring s = (*env)->NewStringUTF(env, buf);
    free(buf);
    return s;
}

JNIEXPORT jstring JNICALL JNI_FN(getParamsRon)(JNIEnv *env, jclass cls) {
    (void)cls;
    return take_string(env, asmr_get_params_ron);
}

JNIEXPORT jstring JNICALL JNI_FN(lastError)(JNIEnv *env, jclass cls) {
    (void)cls;
    return take_string(env, asmr_last_error);
}

JNIEXPORT jint JNICALL JNI_FN(action)(JNIEnv *env, jclass cls, jstring name) {
    (void)cls;
    const char *s = (*env)->GetStringUTFChars(env, name, NULL);
    if (!s) return -1;
    int r = asmr_action(s);
    (*env)->ReleaseStringUTFChars(env, name, s);
    return r;
}

JNIEXPORT jint JNICALL JNI_FN(setField)(JNIEnv *env, jclass cls, jstring key, jstring value) {
    (void)cls;
    const char *k = (*env)->GetStringUTFChars(env, key, NULL);
    const char *v = (*env)->GetStringUTFChars(env, value, NULL);
    int r = (k && v) ? asmr_set_field(k, v) : -1;
    if (k) (*env)->ReleaseStringUTFChars(env, key, k);
    if (v) (*env)->ReleaseStringUTFChars(env, value, v);
    return r;
}

JNIEXPORT jstring JNICALL JNI_FN(getParamsJson)(JNIEnv *env, jclass cls) {
    (void)cls;
    return take_string(env, asmr_get_params_json);
}

JNIEXPORT jstring JNICALL JNI_FN(uiSpec)(JNIEnv *env, jclass cls) {
    (void)cls;
    return take_string(env, asmr_ui_spec);
}

JNIEXPORT jint JNICALL JNI_FN(loadConfig)(JNIEnv *env, jclass cls, jstring text) {
    (void)cls;
    const char *s = (*env)->GetStringUTFChars(env, text, NULL);
    if (!s) return -1;
    int r = asmr_load_config(s);
    (*env)->ReleaseStringUTFChars(env, text, s);
    return r;
}

/// 只检查不导入（别的 App 用「其他应用打开」传来的文件）：返回 "ok\n概要" / "not\n原因" / "bad\n原因"。
JNIEXPORT jstring JNICALL JNI_FN(checkConfig)(JNIEnv *env, jclass cls, jstring text) {
    (void)cls;
    const char *s = (*env)->GetStringUTFChars(env, text, NULL);
    if (!s) return (*env)->NewStringUTF(env, "bad\n读不了这个文件");
    char buf[1024];
    int r = asmr_check_config(s, buf, sizeof buf - 8);
    (*env)->ReleaseStringUTFChars(env, text, s);
    char out[1100];
    snprintf(out, sizeof out, "%s\n%s", r == 0 ? "ok" : r == 1 ? "not" : "bad", buf);
    return (*env)->NewStringUTF(env, out);
}

JNIEXPORT jstring JNICALL JNI_FN(exportConfig)(JNIEnv *env, jclass cls) {
    (void)cls;
    return take_string(env, asmr_export_config);
}

// 输出效果 / 监听原声：存进配置（和桌面版共用的 dry）。hook 一直交给处理器：原声时 Rust 那边不进引擎，
// 只乘输出增益、过峰值限制器（所以原声时音量滑条照样起作用）；调试用的模式（setMode）回到 ASMR
JNIEXPORT void JNICALL JNI_FN(setDry)(JNIEnv *env, jclass cls, jboolean dry) {
    (void)env;
    (void)cls;
    asmr_set_dry(dry ? 1 : 0);
    atomic_store(&g_mode, MODE_ASMR);
}

// 文件转换页（rust/src/files.rs）：命令和状态都按 UTF-8 字节传（文件名里可能有 JNI 字符串处理不了的字符）
JNIEXPORT jbyteArray JNICALL JNI_FN(files)(JNIEnv *env, jclass cls, jbyteArray cmd) {
    (void)cls;
    jsize n = (*env)->GetArrayLength(env, cmd);
    jbyte *c = (*env)->GetByteArrayElements(env, cmd, NULL);
    if (!c) return NULL;
    size_t out_len = 0;
    uint8_t *out = asmr_files((const uint8_t *)c, (size_t)n, &out_len);
    (*env)->ReleaseByteArrayElements(env, cmd, c, JNI_ABORT);
    if (!out) return NULL;
    jbyteArray a = (*env)->NewByteArray(env, (jsize)out_len);
    if (a) (*env)->SetByteArrayRegion(env, a, 0, (jsize)out_len, (const jbyte *)out);
    asmr_free_bytes(out, out_len);
    return a;
}

JNIEXPORT jfloatArray JNICALL JNI_FN(manualFromPose)(JNIEnv *env, jclass cls, jfloat az, jfloat el, jfloat r) {
    (void)cls;
    float out[3];
    asmr_manual_from_pose(az, el, r, out);
    jfloatArray a = (*env)->NewFloatArray(env, 3);
    if (a) (*env)->SetFloatArrayRegion(env, a, 0, 3, out);
    return a;
}

// 给界面的实时状态：最近 1 秒里还在回调、建了处理器的流中最近出声的那个。
// [0] 有没有 [1] 方位角 [2] 仰角 [3] 距离（米） [4] 说话活动 [5] 处理占用（单核 %） [6] 延迟（ms）
// [7] 断流次数 [8] 采样率 [9] 流编号 [10] 引擎出过错
// [11] 是不是左右双人 [12] 2 号的方位角 [13] 仰角 [14] 距离 [15] 说话活动（左右双人时 [1]–[4] 是 1 号）
#define TELE_N 16
JNIEXPORT jfloatArray JNICALL JNI_FN(telemetry)(JNIEnv *env, jclass cls) {
    (void)cls;
    float out[TELE_N] = {0};
    uint64_t now = now_ns(), best = 0;
    pthread_mutex_lock(&g_report_mu);
    ctx_t *pick = NULL;
    for (int i = 0; i < MAX_CTX; i++) {
        ctx_t *c = &g_ctx[i];
        if (atomic_load(&c->state) != SLOT_ACTIVE || !c->tele) continue;
        uint64_t t = LD(c->last_cb_ns);
        if (t > best && now - t < 1000000000ull) {
            best = t;
            pick = c;
        }
    }
    if (pick) {
        float t[11];
        asmr_tele_read(pick->tele, t);
        uint64_t pn = LD(pick->proc_ns), pf = LD(pick->proc_frames);
        uint64_t dpn = pn - pick->u_proc_ns, dpf = pf - pick->u_proc_frames;
        pick->u_proc_ns = pn;
        pick->u_proc_frames = pf;
        out[0] = 1;
        out[1] = t[0];
        out[2] = t[1];
        out[3] = t[2];
        out[4] = t[3];
        out[5] = dpf ? (float)((double)dpn / ((double)dpf / pick->rate * 1e9) * 100.0) : 0.0f;
        out[6] = pick->latency_ms;
        out[7] = t[4];
        out[8] = (float)pick->rate;
        out[9] = (float)pick->id;
        out[10] = t[5];
        for (int k = 0; k < 5; k++) out[11 + k] = t[6 + k];
    }
    pthread_mutex_unlock(&g_report_mu);
    jfloatArray a = (*env)->NewFloatArray(env, TELE_N);
    if (a) (*env)->SetFloatArrayRegion(env, a, 0, TELE_N, out);
    return a;
}

JNIEXPORT jint JNICALL JNI_FN(record)(JNIEnv *env, jclass cls, jstring dir, jint seconds) {
    (void)cls;
    if (seconds <= 0 || seconds > 600) return -1;
    int expect = REC_IDLE;
    if (!atomic_compare_exchange_strong(&g_rec.state, &expect, REC_DONE)) return -1; // 先占住，准备好再放开
    const char *d = (*env)->GetStringUTFChars(env, dir, NULL);
    snprintf(g_rec.dir, sizeof g_rec.dir, "%s", d ? d : ".");
    if (d) (*env)->ReleaseStringUTFChars(env, dir, d);
    g_rec.seconds = seconds;
    g_rec.cap = (size_t)seconds * 48000;
    g_rec.dry = malloc(g_rec.cap * 2 * sizeof(float));
    g_rec.wet = malloc(g_rec.cap * 2 * sizeof(float));
    if (!g_rec.dry || !g_rec.wet) {
        free(g_rec.dry);
        free(g_rec.wet);
        g_rec.dry = g_rec.wet = NULL;
        atomic_store(&g_rec.state, REC_IDLE);
        return -1;
    }
    atomic_store(&g_rec.n, 0);
    atomic_store(&g_rec.owner, 0);
    atomic_store(&g_rec.rate, 0);
    atomic_store(&g_rec.state, REC_ARMED);
    pthread_t t;
    pthread_create(&t, NULL, rec_writer, NULL);
    pthread_detach(t);
    LOGI("开始录音 %d 秒：下一个出声的流，处理前后各存一份 → %s", seconds, g_rec.dir);
    return 0;
}

JNIEXPORT jstring JNICALL JNI_FN(status)(JNIEnv *env, jclass cls) {
    (void)cls;
    char buf[8192];
    int len;
    pthread_mutex_lock(&g_hook_log_mu);
    len = snprintf(buf, sizeof buf, "已 hook：%s | 模式：%s%s\n", g_hook_log[0] ? g_hook_log : "（WebView 播放过声音后才会出现）",
                   MODE_NAME[atomic_load(&g_mode)], atomic_load(&g_rec.state) != REC_IDLE ? " | 录音中" : "");
    pthread_mutex_unlock(&g_hook_log_mu);
    format_stats(buf + len, sizeof buf - (size_t)len, false);
    return (*env)->NewStringUTF(env, buf);
}
