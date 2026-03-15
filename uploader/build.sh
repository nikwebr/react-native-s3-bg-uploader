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
IOS_RUST_DIR="$SCRIPT_DIR/../ios/rust"

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

    echo -e "${BLUE}🔗 Creating universal simulator fat library (lipo)...${NC}"
    mkdir -p target/universal-ios-sim/release
    lipo -create \
        target/aarch64-apple-ios-sim/release/libuploader.a \
        target/x86_64-apple-ios/release/libuploader.a \
        -output target/universal-ios-sim/release/libuploader.a

    echo -e "${BLUE}📦 Creating XCFramework...${NC}"
    rm -rf target/libuploader.xcframework
    xcodebuild -create-xcframework \
        -library target/aarch64-apple-ios/release/libuploader.a \
        -library target/universal-ios-sim/release/libuploader.a \
        -output target/libuploader.xcframework

    mkdir -p "$IOS_RUST_DIR"

    echo -e "${BLUE}🔤 Generating header with cbindgen...${NC}"
    if command -v cbindgen &> /dev/null; then
        cbindgen --config cbindgen.toml --crate uploader --output "$IOS_RUST_DIR/uploader.h"
        echo -e "${GREEN}✓ Header generated: ios/rust/uploader.h${NC}"
    else
        echo -e "${RED}⚠ cbindgen not found – header not regenerated. Install with:${NC}"
        echo "  cargo install cbindgen"
    fi

    echo -e "${BLUE}📋 Copying artifacts to ios/rust/...${NC}"
    cp target/aarch64-apple-ios/release/libuploader.a "$IOS_RUST_DIR/libuploader.a"

    echo -e "${GREEN}✓ iOS build successful${NC}"
    echo "  Device lib : ios/rust/libuploader.a  (aarch64-apple-ios)"
    echo "  Header     : ios/rust/uploader.h"
    echo "  XCFramework: uploader/target/libuploader.xcframework  (device + sim)"
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
                --out-dir ./pkg \
                --target web \
                ./target/wasm32-unknown-unknown/release/uploader.wasm

            if [ $? -eq 0 ]; then
                echo -e "${GREEN}✓ wasm-bindgen successful${NC}"
                echo "Output: pkg/"
                echo "  - uploader.js"
                echo "  - uploader_bg.wasm"
                echo "  - uploader.d.ts"
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

