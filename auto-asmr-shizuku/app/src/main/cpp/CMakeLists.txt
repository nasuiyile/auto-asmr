cmake_minimum_required(VERSION 3.22.1)
project(asmrshizuku C)

# ---- Rust 引擎：直接用安卓版的 Rust 层（auto-asmr-android/rust，C 接口见 asmr_core.h），编成静态库链进来
set(ANDROID_PROJ ${CMAKE_CURRENT_SOURCE_DIR}/../../../../../auto-asmr-android)
set(RUST_DIR ${ANDROID_PROJ}/rust)
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
# 和安卓版共用 target 目录，没改动时不用重新编译
set(RUST_TARGET_DIR ${RUST_DIR}/target)
set(RUST_LIB ${RUST_TARGET_DIR}/${RUST_TARGET}/release/libasmr_android.a)
add_custom_target(rust_core
    COMMAND ${CARGO} build --release --target ${RUST_TARGET} --manifest-path ${RUST_DIR}/Cargo.toml --target-dir ${RUST_TARGET_DIR}
    BYPRODUCTS ${RUST_LIB}
    WORKING_DIRECTORY ${RUST_DIR}
    COMMENT "cargo build --release --target ${RUST_TARGET}"
    VERBATIM)
add_library(asmr_core STATIC IMPORTED)
set_target_properties(asmr_core PROPERTIES IMPORTED_LOCATION ${RUST_LIB})

# ---- JNI 层：给服务进程（server/Engine.kt）用
add_library(asmrshizuku SHARED asmr_jni.c)
add_dependencies(asmrshizuku rust_core)
target_include_directories(asmrshizuku PRIVATE ${ANDROID_PROJ}/app/src/main/cpp)
target_compile_options(asmrshizuku PRIVATE -Wall -Wextra -O2)
target_link_libraries(asmrshizuku asmr_core log m dl)
target_link_options(asmrshizuku PRIVATE "-Wl,-z,max-page-size=16384" "-Wl,--gc-sections")
