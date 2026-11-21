package io.github.nasuiyile.autoasmr.shizuku

import android.app.Application

/**
 * App 进程启动：加载 Rust 引擎（界面和参数用，和安卓版一样），后台线程里读数据文件和上次保存的配置；
 * 初始化 [Controller]（收到 Shizuku 的 binder 后会连回还在跑的服务进程）。
 */
class AsmrApp : Application() {
    override fun onCreate() {
        super.onCreate()
        System.loadLibrary("asmrshizuku")
        Thread({ Settings.load(this) }, "asmr-init").start()
        Controller.init(this)
    }
}
