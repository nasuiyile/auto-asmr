#!/usr/bin/env bash
# 构建浏览器插件：把 Rust 引擎编成 wasm，放进 extension/。之后在 chrome://extensions 里「加载已解压的扩展程序」选 extension/ 目录。
#
#   ./build.sh          编译 wasm（release）并复制到 extension/asmr.wasm
#   ./build.sh --zip    另外打一个 dist/auto-asmr-chromium.zip（上传 Chrome 应用商店用）
#
# 需要 Rust 和它的 wasm 目标：rustup target add wasm32-unknown-unknown
set -euo pipefail
cd "$(dirname "$0")"

rustup target list --installed 2>/dev/null | grep -q '^wasm32-unknown-unknown$' || {
  echo "缺少 wasm 目标，先运行：rustup target add wasm32-unknown-unknown" >&2
  exit 1
}

(cd wasm && cargo build --release --target wasm32-unknown-unknown)
cp wasm/target/wasm32-unknown-unknown/release/asmr_chromium.wasm extension/asmr.wasm
echo "extension/asmr.wasm $(du -h extension/asmr.wasm | cut -f1)"

if [ "${1:-}" = "--zip" ]; then
  mkdir -p dist
  rm -f dist/auto-asmr-chromium.zip
  (cd extension && zip -qr ../dist/auto-asmr-chromium.zip . -x '.*')
  zip -qj dist/auto-asmr-chromium.zip ../LICENSE
  echo "dist/auto-asmr-chromium.zip $(du -h dist/auto-asmr-chromium.zip | cut -f1)"
fi
