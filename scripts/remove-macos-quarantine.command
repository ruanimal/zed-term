#!/usr/bin/env bash
set -euo pipefail

script_path="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)/$(basename -- "${BASH_SOURCE[0]}")"
if [[ $# -gt 1 ]]; then
    echo "Usage: $(basename -- "$script_path") [path/to/ZedTerm.app]" >&2
    exit 2
fi

if [[ $# -eq 0 ]]; then
    app_path="/Applications/ZedTerm.app"
else
    app_path="$1"
    if [[ "$app_path" != /* ]]; then
        app_path="$(pwd -P)/$app_path"
    fi
fi
app_path="${app_path%/}"

if [[ "$(basename -- "$app_path")" != "ZedTerm.app" ]]; then
    echo "Refusing to modify an application whose name is not ZedTerm.app: $app_path" >&2
    exit 1
fi
if [[ ! -d "$app_path" ]]; then
    echo "Application not found: $app_path" >&2
    exit 1
fi

contents_path="$app_path/Contents"
info_plist="$contents_path/Info.plist"
binary_path="$contents_path/MacOS/terminal-app"
if [[ ! -f "$info_plist" || ! -f "$binary_path" ]]; then
    echo "The application does not have the expected ZedTerm bundle layout: $app_path" >&2
    exit 1
fi

if ! bundle_identifier="$(plutil -extract CFBundleIdentifier raw -o - "$info_plist" 2>/dev/null)"; then
    echo "Unable to read the bundle identifier from $info_plist" >&2
    exit 1
fi
if [[ "$bundle_identifier" != "dev.zed.ZedTerm" ]]; then
    echo "Refusing to modify an application with bundle identifier $bundle_identifier" >&2
    exit 1
fi

if ! sudo /usr/bin/xattr -rd com.apple.quarantine "$app_path"; then
    echo "Unable to remove the macOS quarantine attribute from $app_path" >&2
    exit 1
fi

echo "Removed the macOS quarantine attribute from $app_path"
echo "Only use this script for a ZedTerm application from a trusted source."
