#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "${1:-}" == "--android-only" ]]; then
  cargo ndk -t arm64-v8a --platform 26 build --locked --offline -p chat-ffi --lib
  mkdir -p build/native/android/arm64-v8a
  find build/native/android/arm64-v8a -maxdepth 1 -type f -name 'libiroh*.so' -delete
  cp target/aarch64-linux-android/debug/libchat_ffi.so build/native/android/arm64-v8a/
else
  cargo build --locked --offline -p chat-ffi --lib
  mkdir -p build/native/host build/generated/chat-ffi
  find build/native/host -maxdepth 1 -type f -name 'libiroh*.so' -delete
  cp target/debug/libchat_ffi.so build/native/host/
  cargo run --locked --offline -p chat-ffi --features bindgen --bin chat-bindgen -- generate --library target/debug/libchat_ffi.so --language kotlin --no-format --out-dir build/generated/chat-ffi
fi
