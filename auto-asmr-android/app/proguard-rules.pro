# C 层（app/src/main/cpp/aaudio_hook.c）按名字找这些 JNI 方法：类名、方法名都不能改
-keep class io.github.nasuiyile.autoasmr.NativeHook { *; }

# 全局模式的服务进程按名字加载：app_process 的主类（ServerMain、LatencyProbe）、Shizuku UserService 的类（EngineService，
# Shizuku 用反射建它）；JNI（app/src/main/cpp/global_jni.c）按类名、方法名找 Engine 的方法。整个 server 包都原样保留
-keep class io.github.nasuiyile.autoasmr.global.server.** { *; }
