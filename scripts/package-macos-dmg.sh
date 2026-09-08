#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$(cd -- "$script_directory/.." && pwd)"
package_manifest="$workspace_root/crates/terminal_app/Cargo.toml"

usage() {
    cat <<'USAGE'
Usage: package-macos-dmg.sh [options]

Options:
  --version VERSION       Package version; defaults to terminal_app's Cargo version
  --arch ARCH             x86_64 or arm64; defaults to the host architecture
  --build-number NUMBER   CFBundleVersion; defaults to 1
  --binary PATH           Release binary; defaults to target/release/terminal-app
  --output PATH           DMG path; defaults to target/package/zedterm-VERSION-macos-ARCH-unsigned.dmg
USAGE
}

version=""
architecture=""
build_number="1"
binary_path="$workspace_root/target/release/terminal-app"
output_path=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            if [[ $# -lt 2 ]]; then
                echo "--version requires a value" >&2
                exit 2
            fi
            version="$2"
            shift 2
            ;;
        --arch)
            if [[ $# -lt 2 ]]; then
                echo "--arch requires a value" >&2
                exit 2
            fi
            architecture="$2"
            shift 2
            ;;
        --build-number)
            if [[ $# -lt 2 ]]; then
                echo "--build-number requires a value" >&2
                exit 2
            fi
            build_number="$2"
            shift 2
            ;;
        --binary)
            if [[ $# -lt 2 ]]; then
                echo "--binary requires a path" >&2
                exit 2
            fi
            binary_path="$2"
            shift 2
            ;;
        --output)
            if [[ $# -lt 2 ]]; then
                echo "--output requires a path" >&2
                exit 2
            fi
            output_path="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

package_version="$(awk -F'"' '/^version = "/ { print $2; exit }' "$package_manifest")"
if [[ -z "$package_version" ]]; then
    echo "Unable to determine terminal_app version from $package_manifest" >&2
    exit 1
fi

if [[ -z "$version" ]]; then
    version="$package_version"
elif [[ "$version" != "$package_version" ]]; then
    echo "Package version $version does not match terminal_app Cargo version $package_version" >&2
    exit 1
fi

if [[ "$version" == */* || "$version" == *[[:space:]]* ]]; then
    echo "Package version contains an invalid path character: $version" >&2
    exit 1
fi

if [[ -z "$architecture" ]]; then
    case "$(uname -m)" in
        arm64) architecture="arm64" ;;
        x86_64) architecture="x86_64" ;;
        *)
            echo "Unable to determine a supported macOS architecture from $(uname -m)" >&2
            exit 1
            ;;
    esac
fi
case "$architecture" in
    x86_64|arm64) ;;
    *)
        echo "Unsupported architecture: $architecture (expected x86_64 or arm64)" >&2
        exit 2
        ;;
esac

if [[ -z "$build_number" || "$build_number" == *[!0-9]* ]]; then
    echo "Build number must contain only digits: $build_number" >&2
    exit 2
fi

if [[ "$binary_path" != /* ]]; then
    binary_path="$(pwd)/$binary_path"
fi
if [[ -z "$output_path" ]]; then
    output_path="$workspace_root/target/package/zedterm-${version}-macos-${architecture}-unsigned.dmg"
elif [[ "$output_path" != /* ]]; then
    output_path="$(pwd)/$output_path"
fi

if [[ ! -x "$binary_path" ]]; then
    echo "Release binary not found at $binary_path" >&2
    echo "Run: cargo build --locked --release -p terminal_app --bin terminal-app" >&2
    exit 1
fi
source_icon="$workspace_root/crates/terminal_app/resources/app-icon@2x.png"
if [[ ! -f "$source_icon" ]]; then
    echo "Application icon not found at $source_icon" >&2
    exit 1
fi

mkdir -p "$(dirname -- "$output_path")"
temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/zedterm-package.XXXXXX")"
trap 'rm -rf "$temporary_directory"' EXIT

app_path="$temporary_directory/ZedTerm.app"
contents_path="$app_path/Contents"
iconset_path="$temporary_directory/ZedTerm.iconset"
rm -f "$output_path"
mkdir -p "$contents_path/MacOS" "$contents_path/Resources" "$iconset_path"

cp "$binary_path" "$contents_path/MacOS/terminal-app"
chmod 755 "$contents_path/MacOS/terminal-app"

for icon_size in 16 32 128 256 512; do
    sips -s format png -z "$icon_size" "$icon_size" "$source_icon" \
        --out "$iconset_path/icon_${icon_size}x${icon_size}.png" >/dev/null
    retina_size=$((icon_size * 2))
    sips -s format png -z "$retina_size" "$retina_size" "$source_icon" \
        --out "$iconset_path/icon_${icon_size}x${icon_size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset_path" -o "$contents_path/Resources/ZedTerm.icns"

short_version="${version%%-*}"
cat > "$contents_path/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDisplayName</key>
    <string>ZedTerm</string>
    <key>CFBundleExecutable</key>
    <string>terminal-app</string>
    <key>CFBundleIconFile</key>
    <string>ZedTerm</string>
    <key>CFBundleIdentifier</key>
    <string>dev.zed.ZedTerm</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>ZedTerm</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$short_version</string>
    <key>CFBundleSignature</key>
    <string>ZDTR</string>
    <key>CFBundleVersion</key>
    <string>$build_number</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.15.7</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
PLIST

plutil -lint "$contents_path/Info.plist"
test -f "$contents_path/Resources/ZedTerm.icns"
hdiutil create -volname "ZedTerm $version" -srcfolder "$app_path" \
    -ov -format UDZO "$output_path"
hdiutil imageinfo "$output_path" >/dev/null

printf 'Created %s\n' "$output_path"
