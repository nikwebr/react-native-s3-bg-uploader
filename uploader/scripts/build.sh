#!/bin/bash

# Build script for iOS and WASM

set -e

echo "🔧 Building Uploader Library..."
echo ""

GREEN='\033[0;32m'
BLUE='\033[0;34m'
RED='\033[0;31m'
NC='\033[0m' # No Color

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_RUST_DIR="$SCRIPT_DIR/../../ios/rust"
TARGET_DIR="$SCRIPT_DIR/../target"
IOS_MIN_VERSION="15.1" # same as React Native's min_ios_version_supported

ensure_deps() {
    echo -e "${BLUE}🔍 Checking dependencies...${NC}"

    # rustup
    if ! command -v rustup &> /dev/null; then
        echo -e "${RED}✗ rustup not found. Install from https://rustup.rs${NC}"
        exit 1
    fi

    # Rust toolchain (cargo)
    if ! command -v cargo &> /dev/null; then
        echo -e "${BLUE}  No Rust toolchain found – installing stable...${NC}"
        rustup toolchain install stable
        rustup default stable
    else
        echo -e "${GREEN}✓ cargo $(cargo --version | awk '{print $2}')${NC}"
    fi

    echo ""
}

ensure_ios_deps() {
    # iOS Rust targets
    local ios_targets=(
        "aarch64-apple-ios"
        "aarch64-apple-ios-sim"
        "x86_64-apple-ios"
    )
    local installed_targets
    installed_targets=$(rustup target list --installed)
    for target in "${ios_targets[@]}"; do
        if echo "$installed_targets" | grep -q "^$target$"; then
            echo -e "${GREEN}✓ $target${NC}"
        else
            echo -e "${BLUE}  Installing $target...${NC}"
            rustup target add "$target"
        fi
    done

    # cbindgen
    if command -v cbindgen &> /dev/null; then
        echo -e "${GREEN}✓ cbindgen$(cbindgen --version | awk '{print " "$2}')${NC}"
    else
        echo -e "${BLUE}  Installing cbindgen...${NC}"
        cargo install cbindgen
    fi

    echo ""
}


if [[ "$1" == "ios" ]] || [[ "$1" == "wasm" ]] || [[ "$1" == "all" ]]; then
    ensure_deps
fi

if [[ "$1" == "ios" ]] || [[ "$1" == "all" ]]; then
    ensure_ios_deps
fi

# iOS Build (Device + Simulator, universal fat library)
if [[ "$1" == "ios" ]] || [[ "$1" == "all" ]]; then
    echo -e "${BLUE}📱 Building for iOS device (aarch64-apple-ios)...${NC}"
    cargo build --features ios --target aarch64-apple-ios --release

    echo -e "${BLUE}📱 Building for iOS Simulator arm64 (aarch64-apple-ios-sim)...${NC}"
    cargo build --features ios --target aarch64-apple-ios-sim --release

    echo -e "${BLUE}📱 Building for iOS Simulator x86_64 (x86_64-apple-ios)...${NC}"
    cargo build --features ios --target x86_64-apple-ios --release

    mkdir -p "$IOS_RUST_DIR"

    echo -e "${BLUE}🔤 Generating header with cbindgen...${NC}"
    if command -v cbindgen &> /dev/null; then
        cbindgen --config cbindgen.toml --crate uploader --output "$IOS_RUST_DIR/uploader.h"
        echo -e "${GREEN}✓ Header generated: ios/rust/uploader.h${NC}"
    else
        echo -e "${RED}⚠ cbindgen not found – header not regenerated. Install with:${NC}"
        echo "  cargo install cbindgen"
    fi

    echo -e "${BLUE}🔧 Fixing dylib install names...${NC}"
    install_name_tool -id "@rpath/libuploader.framework/libuploader" \
        $TARGET_DIR/aarch64-apple-ios/release/libuploader.dylib
    install_name_tool -id "@rpath/libuploader.framework/libuploader" \
        $TARGET_DIR/aarch64-apple-ios-sim/release/libuploader.dylib
    install_name_tool -id "@rpath/libuploader.framework/libuploader" \
        $TARGET_DIR/x86_64-apple-ios/release/libuploader.dylib

    echo -e "${BLUE}🔗 Creating universal simulator fat dylib (lipo)...${NC}"
    mkdir -p $TARGET_DIR/universal-ios-sim/release
    lipo -create \
        $TARGET_DIR/aarch64-apple-ios-sim/release/libuploader.dylib \
        $TARGET_DIR/x86_64-apple-ios/release/libuploader.dylib \
        -output $TARGET_DIR/universal-ios-sim/release/libuploader.dylib

    echo -e "${BLUE}🏗️  Creating .framework bundles...${NC}"

    make_framework() {
        local DYLIB="$1"
        local OUT_FW="$2"
        local MIN_OS="$3"

        rm -rf "$OUT_FW"
        mkdir -p "$OUT_FW"
        cp "$DYLIB" "$OUT_FW/libuploader"
        cat > "$OUT_FW/Info.plist" << PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>libuploader</string>
    <key>CFBundleIdentifier</key>
    <string>com.rust.libuploader</string>
    <key>CFBundleName</key>
    <string>libuploader</string>
    <key>CFBundlePackageType</key>
    <string>FMWK</string>
    <key>CFBundleVersion</key>
    <string>1.0</string>
    <key>CFBundleShortVersionString</key>
    <string>1.0</string>
    <key>MinimumOSVersion</key>
    <string>$MIN_OS</string>
    <key>CFBundleSupportedPlatforms</key>
    <array>
        <string>iPhoneOS</string>
    </array>
</dict>
</plist>
PLISTEOF
    }

    FW_DIR="$TARGET_DIR/frameworks"
    rm -rf "$FW_DIR"

    make_framework \
        "$TARGET_DIR/aarch64-apple-ios/release/libuploader.dylib" \
        "$FW_DIR/ios-arm64/libuploader.framework" \
        "$IOS_MIN_VERSION"

    make_framework \
        "$TARGET_DIR/universal-ios-sim/release/libuploader.dylib" \
        "$FW_DIR/ios-sim/libuploader.framework" \
        "$IOS_MIN_VERSION"

    echo -e "${BLUE}📦 Creating XCFramework...${NC}"
    rm -rf $TARGET_DIR/libuploader.xcframework
    xcodebuild -create-xcframework \
        -framework "$FW_DIR/ios-arm64/libuploader.framework" \
        -framework "$FW_DIR/ios-sim/libuploader.framework" \
        -output $TARGET_DIR/libuploader.xcframework

    echo -e "${BLUE}📋 Copying artifacts to ios/rust/...${NC}"
    rm -rf "$IOS_RUST_DIR/libuploader.xcframework"
    cp -R $TARGET_DIR/libuploader.xcframework "$IOS_RUST_DIR/libuploader.xcframework"

    echo -e "${GREEN}✓ iOS build successful${NC}"
    echo "  XCFramework: ios/rust/libuploader.xcframework  (device + simulator)"
    echo "  Header     : ios/rust/uploader.h"
    echo ""
fi

# WASM Build
if [[ "$1" == "wasm" ]] || [[ "$1" == "all" ]]; then
    echo -e "${BLUE}🌐 Building for WASM...${NC}"
    cargo build --features wasm --target wasm32-unknown-unknown --release

    if [ $? -eq 0 ]; then
        echo -e "${GREEN}✓ WASM build successful${NC}"

        # wasm-bindgen prüfen
        if command -v wasm-bindgen &> /dev/null; then
            echo -e "${BLUE}🔗 Running wasm-bindgen...${NC}"
            wasm-bindgen \
                --out-dir $TARGET_DIR/wasm \
                --target no-modules \
                $TARGET_DIR/wasm32-unknown-unknown/release/uploader.wasm

            if [ $? -eq 0 ]; then
                echo -e "${GREEN}✓ wasm-bindgen successful${NC}"
                echo "Output: target/wasm/"
                echo "  - uploader.js"
                echo "  - uploader_bg.wasm"
                echo "  - uploader.d.ts"

                echo -e "${BLUE}📦 Embedding WASM into library source...${NC}"
                node "$SCRIPT_DIR/embed-wasm.js"
                echo -e "${GREEN}✓ WASM embedded into src/web/wasm-assets.ts${NC}"
            else
                echo -e "${RED}✗ wasm-bindgen failed${NC}"
                exit 1
            fi
        else
            echo -e "${RED}⚠ wasm-bindgen not found. Install with:${NC}"
            echo "  cargo install wasm-bindgen-cli"
        fi
    else
        echo -e "${RED}✗ WASM build failed${NC}"
        exit 1
    fi
    echo ""
fi

# Usage
if [[ -z "$1" ]] || [[ "$1" == "help" ]]; then
    echo "Usage: ./build.sh [ios|wasm|all]"
    echo ""
    echo "Examples:"
    echo "  ./build.sh ios    - Build only for iOS"
    echo "  ./build.sh wasm   - Build only for WASM"
    echo "  ./build.sh all    - Build for both platforms"
    exit 0
fi

echo -e "${GREEN}✅ Build completed successfully!${NC}"

