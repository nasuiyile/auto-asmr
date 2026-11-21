# 服务进程的代码按名字加载：app_process 的主类（ServerMain、LatencyProbe）、Shizuku UserService 的类（EngineService，
# Shizuku 用反射建它）；JNI（app/src/main/cpp/asmr_jni.c）按类名、方法名找 Engine 的方法。整个 server 包都原样保留
-keep class io.github.nasuiyile.autoasmr.shizuku.server.** { *; }
