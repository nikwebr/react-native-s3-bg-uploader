#!/bin/bash
set -e

# Auto-detect project directory (script is in project root)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LIB_DIR="$SCRIPT_DIR/custom-encryptor-rust"
EXAMPLE_IOS="$SCRIPT_DIR/example-app/ios"
XCFRAMEWORK_DIR="$EXAMPLE_IOS/Libraries/libcustom_encryptor.xcframework"

echo "=== Building Custom Encryptor ==="
echo "Project:  $SCRIPT_DIR"
echo "Library:  $LIB_DIR"
echo "Example:  $EXAMPLE_IOS"

# Ensure iOS targets are installed
echo "Checking Rust iOS targets..."
rustup target list --installed | grep -q "aarch64-apple-ios$" || rustup target add aarch64-apple-ios
rustup target list --installed | grep -q "aarch64-apple-ios-sim" || rustup target add aarch64-apple-ios-sim
rustup target list --installed | grep -q "x86_64-apple-ios" || rustup target add x86_64-apple-ios

# Build all targets
echo "Building for iOS device (aarch64)..."
cargo build --release --target aarch64-apple-ios --manifest-path "$LIB_DIR/Cargo.toml"

echo "Building for iOS simulator (aarch64)..."
cargo build --release --target aarch64-apple-ios-sim --manifest-path "$LIB_DIR/Cargo.toml"

echo "Building for iOS simulator (x86_64)..."
cargo build --release --target x86_64-apple-ios --manifest-path "$LIB_DIR/Cargo.toml"

# Create universal simulator library
echo "Creating universal simulator library..."
UNIVERSAL_SIM_DIR="$LIB_DIR/target/universal-ios-sim/release"
mkdir -p "$UNIVERSAL_SIM_DIR"
lipo -create \
    "$LIB_DIR/target/aarch64-apple-ios-sim/release/libcustom_encryptor.a" \
    "$LIB_DIR/target/x86_64-apple-ios/release/libcustom_encryptor.a" \
    -output "$UNIVERSAL_SIM_DIR/libcustom_encryptor.a"

# Build XCFramework (device + simulator slices in one bundle)
echo "Creating XCFramework..."
mkdir -p "$EXAMPLE_IOS/Libraries"
rm -rf "$XCFRAMEWORK_DIR"
xcodebuild -create-xcframework \
    -library "$LIB_DIR/target/aarch64-apple-ios/release/libcustom_encryptor.a" \
    -library "$UNIVERSAL_SIM_DIR/libcustom_encryptor.a" \
    -output "$XCFRAMEWORK_DIR"

echo "=== Build Complete ==="
echo "XCFramework: $XCFRAMEWORK_DIR"
echo ""
echo "To build the example app:"
echo "  cd $EXAMPLE_IOS && pod install"
echo "  xcodebuild -scheme CustomEncryptionExample \\"
echo "      -workspace CustomEncryptionExample.xcworkspace \\"
echo "      -destination 'platform=iOS Simulator,name=iPhone 17' \\"
echo "      -configuration Debug build"