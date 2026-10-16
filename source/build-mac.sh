#!/bin/sh
# Builds the macOS player. Run on a Mac with Rust installed (https://rustup.rs).
#   sh build-mac.sh              builds for this Mac only
#   sh build-mac.sh universal    one file that runs on Apple Silicon and Intel
# Result: ./Xenogears (put it beside the GameData and Saves folders and run it).
set -e
cd "$(dirname "$0")"

# rustup puts cargo in ~/.cargo/bin; a Terminal window that was open during the install does not have it on its path yet
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export PATH

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo was not found. Looked in: $HOME/.cargo/bin, /opt/homebrew/bin, /usr/local/bin"
    echo "Install Rust from https://rustup.rs, then open a new Terminal window and run this again."
    exit 1
fi
echo "using $(command -v cargo): $(cargo --version)"
if ! command -v cc >/dev/null 2>&1; then
    echo "No C compiler/linker found. Run: xcode-select --install"
    exit 1
fi
if [ ! -f Cargo.toml ] || [ ! -f src/main.rs ]; then
    echo "Cargo.toml or src/main.rs is missing in $(pwd); unzip the whole package and run this inside it."
    exit 1
fi

if [ "$1" = "universal" ]; then
    command -v rustup >/dev/null 2>&1 || { echo "universal needs rustup (https://rustup.rs); without it run: sh build-mac.sh"; exit 1; }
    rustup target add aarch64-apple-darwin x86_64-apple-darwin
    cargo build --release --target aarch64-apple-darwin
    cargo build --release --target x86_64-apple-darwin
    lipo -create -output Xenogears target/aarch64-apple-darwin/release/xenogears target/x86_64-apple-darwin/release/xenogears
else
    cargo build --release
    cp target/release/xenogears Xenogears
fi
chmod +x Xenogears
echo "built $(pwd)/Xenogears"
