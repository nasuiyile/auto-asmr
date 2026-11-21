// server/Engine.kt 调用的 JNI 函数：把安卓版 Rust 层的 C 接口（asmr_core.h）原样转过去。
// 两个进程都加载它：服务进程用它处理声音、放雨声；App 进程用它管参数（和安卓版的 Settings 一样）、生成界面。
#include <jni.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#include "asmr_core.h"

#define JNI_FN(name) Java_io_github_nasuiyile_autoasmr_shizuku_server_Engine_##name

JNIEXPORT jint JNICALL JNI_FN(provideData)(JNIEnv *env, jclass cls, jbyteArray ku100, jbyteArray pinna) {
    (void)cls;
    jsize kl = (*env)->GetArrayLength(env, ku100), pl = (*env)->GetArrayLength(env, pinna);
    jbyte *k = (*env)->GetByteArrayElements(env, ku100, NULL);
    jbyte *p = (*env)->GetByteArrayElements(env, pinna, NULL);
    int r = asmr_provide_data((const uint8_t *)k, (size_t)kl, (const uint8_t *)p, (size_t)pl);
    (*env)->ReleaseByteArrayElements(env, ku100, k, JNI_ABORT);
    (*env)->ReleaseByteArrayElements(env, pinna, p, JNI_ABORT);
    return r;
}

JNIEXPORT jint JNICALL JNI_FN(initEngine)(JNIEnv *env, jclass cls) {
    (void)env, (void)cls;
    return asmr_init();
}

JNIEXPORT jlong JNICALL JNI_FN(procNew)(JNIEnv *env, jclass cls, jint sampleRate, jint channels) {
    (void)env, (void)cls;
    return (jlong)(intptr_t)asmr_proc_new(sampleRate, channels);
}

JNIEXPORT void JNICALL JNI_FN(procFree)(JNIEnv *env, jclass cls, jlong p) {
    (void)env, (void)cls;
    asmr_proc_free((AsmrProcessor *)(intptr_t)p);
}

// 原地处理交错立体声 float；返回 -1 表示出过错（之后 Rust 那边一律直通）
JNIEXPORT jint JNICALL JNI_FN(procProcess)(JNIEnv *env, jclass cls, jlong p, jfloatArray data, jint frames) {
    (void)cls;
    float *d = (*env)->GetPrimitiveArrayCritical(env, data, NULL);
    if (d == NULL) return -1;
    int r = asmr_proc_process((AsmrProcessor *)(intptr_t)p, d, frames);
    (*env)->ReleasePrimitiveArrayCritical(env, data, d, 0);
    return r;
}

JNIEXPORT jfloat JNICALL JNI_FN(procLatencyMs)(JNIEnv *env, jclass cls, jlong p) {
    (void)env, (void)cls;
    return asmr_proc_latency_ms((const AsmrProcessor *)(intptr_t)p);
}

JNIEXPORT void JNICALL JNI_FN(setDry)(JNIEnv *env, jclass cls, jboolean dry) {
    (void)env, (void)cls;
    asmr_set_dry(dry ? 1 : 0);
}

JNIEXPORT jint JNICALL JNI_FN(setStyle)(JNIEnv *env, jclass cls, jint style) {
    (void)env, (void)cls;
    return asmr_set_style(style);
}

static jint with_utf(JNIEnv *env, jstring s, int (*fn)(const char *)) {
    const char *c = (*env)->GetStringUTFChars(env, s, NULL);
    if (c == NULL) return -1;
    int r = fn(c);
    (*env)->ReleaseStringUTFChars(env, s, c);
    return r;
}

JNIEXPORT jint JNICALL JNI_FN(action)(JNIEnv *env, jclass cls, jstring name) {
    (void)cls;
    return with_utf(env, name, asmr_action);
}

JNIEXPORT jint JNICALL JNI_FN(loadConfig)(JNIEnv *env, jclass cls, jstring text) {
    (void)cls;
    return with_utf(env, text, asmr_load_config);
}

JNIEXPORT jstring JNICALL JNI_FN(lastError)(JNIEnv *env, jclass cls) {
    (void)cls;
    char buf[1024];
    if (asmr_last_error(buf, sizeof buf) <= 0) return NULL;
    return (*env)->NewStringUTF(env, buf);
}

// ---- 参数（App 进程里用，和安卓版 NativeHook 的同名函数一样）

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

JNIEXPORT jint JNICALL JNI_FN(setParamsRon)(JNIEnv *env, jclass cls, jstring ron) {
    (void)cls;
    return with_utf(env, ron, asmr_set_params_ron);
}

JNIEXPORT jstring JNICALL JNI_FN(getParamsRon)(JNIEnv *env, jclass cls) {
    (void)cls;
    return take_string(env, asmr_get_params_ron);
}

JNIEXPORT jstring JNICALL JNI_FN(exportConfig)(JNIEnv *env, jclass cls) {
    (void)cls;
    return take_string(env, asmr_export_config);
}

/// 只检查不导入：返回 "ok\n概要" / "not\n原因" / "bad\n原因"。
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

JNIEXPORT jfloatArray JNICALL JNI_FN(manualFromPose)(JNIEnv *env, jclass cls, jfloat az, jfloat el, jfloat r) {
    (void)cls;
    float out[3];
    asmr_manual_from_pose(az, el, r, out);
    jfloatArray a = (*env)->NewFloatArray(env, 3);
    if (a) (*env)->SetFloatArrayRegion(env, a, 0, 3, out);
    return a;
}

// ---- 服务进程：处理器状态、背景雨声

/// 处理器此刻的状态（11 个数，含义见 asmr_core.h 的 asmr_tele_read）。调用方保证处理器还没释放。
JNIEXPORT jfloatArray JNICALL JNI_FN(procTele)(JNIEnv *env, jclass cls, jlong p) {
    (void)cls;
    float out[11] = {0};
    const AsmrTele *t = asmr_proc_tele((const AsmrProcessor *)(intptr_t)p);
    if (t) asmr_tele_read(t, out);
    jfloatArray a = (*env)->NewFloatArray(env, 11);
    if (a) (*env)->SetFloatArrayRegion(env, a, 0, 11, out);
    return a;
}

JNIEXPORT jlong JNICALL JNI_FN(rainNew)(JNIEnv *env, jclass cls) {
    (void)env, (void)cls;
    return (jlong)(intptr_t)asmr_rain_new();
}

JNIEXPORT void JNICALL JNI_FN(rainFree)(JNIEnv *env, jclass cls, jlong p) {
    (void)env, (void)cls;
    asmr_rain_free((void *)(intptr_t)p);
}

/// 生成 frames 帧 48 kHz 立体声：返回 1 = 还有声音，0 = 没声音了，-1 = 出错（写入静音）。
JNIEXPORT jint JNICALL JNI_FN(rainRender)(JNIEnv *env, jclass cls, jlong p, jfloatArray data, jint frames) {
    (void)cls;
    float *d = (*env)->GetPrimitiveArrayCritical(env, data, NULL);
    if (d == NULL) return -1;
    int r = asmr_rain_render((void *)(intptr_t)p, d, frames);
    (*env)->ReleasePrimitiveArrayCritical(env, data, d, 0);
    return r;
}

JNIEXPORT void JNICALL JNI_FN(rainSetOn)(JNIEnv *env, jclass cls, jlong p, jboolean on) {
    (void)env, (void)cls;
    asmr_rain_set_on((void *)(intptr_t)p, on ? 1 : 0);
}
