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
bundle_names=("V2t.app" "V2t Record.app")
bundle_ids=("com.lawdalana.v2t" "com.lawdalana.v2t.record")

MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --release --locked \
    --manifest-path "$rust_dir/Cargo.toml" --target-dir "$target_dir" --target "$target"

mkdir -p "$publish_dir"
lock="$publish_dir/.v2t-macos-package-lock"
if ! mkdir "$lock"; then
    printf 'Could not lock the V2t.app output directory.\n' >&2
    exit 1
fi

stage=
publishing=0
cleanup() {
    if [[ -n $stage ]]; then
        local name failed=0
        if (( publishing )); then
            for name in "${bundle_names[@]}"; do
                if [[ ! -e $stage/$name && -d $publish_dir/$name ]]; then
                    mv "$publish_dir/$name" "$stage/$name" || failed=1
                fi
                if [[ -d $stage/previous-$name ]]; then
                    if [[ -e $publish_dir/$name || -L $publish_dir/$name ]] || ! mv "$stage/previous-$name" "$publish_dir/$name"; then
                        failed=1
                    fi
                fi
            done
        fi
        if (( failed )); then
            printf 'V2t builds and backups preserved at %s\n' "$stage" >&2
        else
            rm -rf "$stage"
        fi
    fi
    rmdir "$lock"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for index in "${!bundle_names[@]}"; do
    name="${bundle_names[$index]}"
    bundle="$publish_dir/$name"
    if [[ -L $bundle || ( -e $bundle && ( ! -d $bundle ||
        $(plutil -extract CFBundleIdentifier raw -o - "$bundle/Contents/Info.plist" 2>/dev/null) != "${bundle_ids[$index]}" ) ) ]]; then
        printf 'Cannot replace the existing path %s\n' "$bundle" >&2
        exit 1
    fi
done

stage=$(mktemp -d "$publish_dir/.v2t-macos.XXXXXX")
staged_bundle="$stage/V2t.app"
staged_launcher="$stage/V2t Record.app"
resources="$staged_bundle/Contents/Resources"
launcher_resources="$staged_launcher/Contents/Resources"
mkdir -p "$staged_bundle/Contents/MacOS" "$resources" "$staged_launcher/Contents/MacOS" "$launcher_resources"
install -m 755 "$target_dir/$target/release/V2t" "$staged_bundle/Contents/MacOS/V2t"
whisper_server=${V2T_BUNDLE_WHISPER_SERVER:-"$publish_dir/V2t.app/Contents/MacOS/whisper-server"}
if [[ -n ${V2T_BUNDLE_WHISPER_SERVER:-} && ( ! -f $whisper_server || ! -x $whisper_server ) ]]; then
    printf 'V2T_BUNDLE_WHISPER_SERVER must point to an executable file.\n' >&2
    exit 1
fi
if [[ -f $whisper_server && -x $whisper_server ]]; then
    install -m 755 "$whisper_server" "$staged_bundle/Contents/MacOS/whisper-server"
fi
install -m 644 "$script_dir/Info.plist" "$staged_bundle/Contents/Info.plist"
install -m 755 "$target_dir/$target/release/V2t" "$staged_launcher/Contents/MacOS/V2t"
install -m 644 "$script_dir/RecordInfo.plist" "$staged_launcher/Contents/Info.plist"
sips -s format icns "$repo_dir/app/V2t.Windows/Assets/v2t.ico" --out "$resources/V2t.icns" >/dev/null
install -m 644 "$resources/V2t.icns" "$launcher_resources/V2t.icns"
: > "$resources/Record.v2t-action"
codesign --force --sign - --options runtime \
    --entitlements "$script_dir/Entitlements.plist" "$staged_bundle"
codesign --verify --strict "$staged_bundle"
codesign --force --sign - --options runtime "$staged_launcher"
codesign --verify --strict "$staged_launcher"

publishing=1
for name in "${bundle_names[@]}"; do
    bundle="$publish_dir/$name"
    if [[ -e $bundle ]]; then
        mv "$bundle" "$stage/previous-$name"
    fi
    mv "$stage/$name" "$bundle"
done
publishing=0
for name in "${bundle_names[@]}"; do
    printf 'Built %s\n' "$publish_dir/$name"
done
