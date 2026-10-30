#!/usr/bin/env bash
# Builds mudling-ffi for iOS: a static library per target, the Swift
# bindings, and an XCFramework holding both libraries.
#
#   scripts/build-ios.sh [--debug]
#
# Output lands in target/ios/: MudlingFFI.xcframework and swift/mudling_ffi.swift.
# Needs Xcode and the Rust targets aarch64-apple-ios and aarch64-apple-ios-sim.
set -euo pipefail

cd "$(dirname "$0")/.."

profile=release
cargo_profile=(--release)
if [[ "${1:-}" == "--debug" ]]; then
  profile=debug
  cargo_profile=()
fi

targets=(aarch64-apple-ios aarch64-apple-ios-sim)
out=target/ios
library=libmudling_ffi.a

for target in "${targets[@]}"; do
  cargo build -p mudling-ffi --lib --target "$target" "${cargo_profile[@]}"
done

rm -rf "$out"
mkdir -p "$out/swift" "$out/headers"
cargo run -p mudling-ffi --features bindgen --bin uniffi-bindgen -- \
  generate --library "target/${targets[0]}/$profile/$library" \
  --language swift --out-dir "$out/swift"

# The XCFramework carries the C header; Swift code imports it as a module.
mv "$out/swift/mudling_ffiFFI.h" "$out/headers/"
mv "$out/swift/mudling_ffiFFI.modulemap" "$out/headers/module.modulemap"

xcframework_args=()
for target in "${targets[@]}"; do
  xcframework_args+=(-library "target/$target/$profile/$library" -headers "$out/headers")
done
xcodebuild -create-xcframework "${xcframework_args[@]}" -output "$out/MudlingFFI.xcframework"

echo "Built $out/MudlingFFI.xcframework and $out/swift/mudling_ffi.swift"
