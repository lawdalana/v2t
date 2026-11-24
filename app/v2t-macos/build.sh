#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Darwin ]]; then
    printf 'Build V2t.app on macOS with Rust and the Xcode command-line tools.\n' >&2
    exit 1
fi

case $(uname -m) in
    arm64) target=aarch64-apple-darwin ;;
    x86_64) target=x86_64-apple-darwin ;;
    *) printf 'Unsupported Mac architecture.\n' >&2; exit 1 ;;
esac

script_dir=$(cd "$(dirname "$0")" && pwd -P)
repo_dir=$(cd "$script_dir/../.." && pwd -P)
rust_dir="$repo_dir/app/v2t-rust"
target_dir="$rust_dir/target"
publish_dir="$repo_dir/publish"
bundle="$publish_dir/V2t.app"

MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --release --locked \
    --manifest-path "$rust_dir/Cargo.toml" --target-dir "$target_dir" --target "$target"

mkdir -p "$publish_dir"
lock="$publish_dir/.v2t-macos-package-lock"
if ! mkdir "$lock"; then
    printf 'Could not lock the V2t.app output directory.\n' >&2
    exit 1
fi

stage=
cleanup() {
    if [[ -n $stage ]]; then
        if [[ -d $stage/previous.app && ! -e $bundle ]]; then
            if ! mv "$stage/previous.app" "$bundle"; then
                printf 'Previous V2t.app preserved at %s\n' "$stage/previous.app" >&2
                rmdir "$lock"
                return
            fi
        fi
        rm -rf "$stage"
    fi
    rmdir "$lock"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

stage=$(mktemp -d "$publish_dir/.v2t-macos.XXXXXX")
staged_bundle="$stage/V2t.app"
mkdir -p "$staged_bundle/Contents/MacOS"
install -m 755 "$target_dir/$target/release/V2t" "$staged_bundle/Contents/MacOS/V2t"
install -m 644 "$script_dir/Info.plist" "$staged_bundle/Contents/Info.plist"
codesign --force --sign - --options runtime \
    --entitlements "$script_dir/Entitlements.plist" "$staged_bundle"
codesign --verify --strict "$staged_bundle"

if [[ -e $bundle || -L $bundle ]]; then
    if [[ ! -d $bundle || -L $bundle || ! -f $bundle/Contents/Info.plist ]]; then
        printf 'Cannot replace the existing path %s\n' "$bundle" >&2
        exit 1
    fi
    mv "$bundle" "$stage/previous.app"
fi
mv "$staged_bundle" "$bundle"
printf 'Built %s\n' "$bundle"
