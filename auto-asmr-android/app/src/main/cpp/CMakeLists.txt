cmake_minimum_required(VERSION 3.22.1)
project(asmrhook C)

find_package(bytehook REQUIRED CONFIG)
find_package(shadowhook REQUIRED CONFIG)

# ---- Rust 层：用 cargo 编成静态库（auto-asmr-android/rust），链进 libasmrhook.so
set(RUST_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../../../../rust)
if(ANDROID_ABI STREQUAL "arm64-v8a")
    set(RUST_TARGET aarch64-linux-android)
elseif(ANDROID_ABI STREQUAL "armeabi-v7a")
    set(RUST_TARGET armv7-linux-androideabi)
elseif(ANDROID_ABI STREQUAL "x86_64")
    set(RUST_TARGET x86_64-linux-android)
else()
    message(FATAL_ERROR "不支持的 ABI：${ANDROID_ABI}")
endif()
find_program(CARGO cargo HINTS $ENV{HOME}/.cargo/bin REQUIRED)
set(RUST_TARGET_DIR ${RUST_DIR}/target)
set(RUST_LIB ${RUST_TARGET_DIR}/${RUST_TARGET}/release/libasmr_android.a)
# 每次构建都调一次 cargo（没改动时很快）；实时处理用的引擎一律按 release 编译
add_custom_target(rust_core
    COMMAND ${CARGO} build --release --target ${RUST_TARGET} --manifest-path ${RUST_DIR}/Cargo.toml --target-dir ${RUST_TARGET_DIR}
    BYPRODUCTS ${RUST_LIB}
    WORKING_DIRECTORY ${RUST_DIR}
    COMMENT "cargo build --release --target ${RUST_TARGET}"
    VERBATIM)
add_library(asmr_core STATIC IMPORTED)
set_target_properties(asmr_core PROPERTIES IMPORTED_LOCATION ${RUST_LIB})

# ---- hook 层
add_library(asmrhook SHARED aaudio_hook.c)
add_dependencies(asmrhook rust_core)
target_compile_options(asmrhook PRIVATE -Wall -Wextra -O2)
target_link_libraries(asmrhook bytehook::bytehook shadowhook::shadowhook asmr_core aaudio log m dl)
# Google Play 要求 16 KB 页对齐（NDK r28 起默认就是，这里写明）；去掉没用到的 Rust 代码
target_link_options(asmrhook PRIVATE "-Wl,-z,max-page-size=16384" "-Wl,--gc-sections")

# ---- 全局模式的服务进程（global/server/Engine.kt）用的 JNI 层：同一个 Rust 静态库，不带 hook。
# 服务进程是 app_process 起的（shell / root 身份），按路径单独加载它
add_library(asmrglobal SHARED global_jni.c)
add_dependencies(asmrglobal rust_core)
target_compile_options(asmrglobal PRIVATE -Wall -Wextra -O2)
target_link_libraries(asmrglobal asmr_core log m dl)
target_link_options(asmrglobal PRIVATE "-Wl,-z,max-page-size=16384" "-Wl,--gc-sections")
