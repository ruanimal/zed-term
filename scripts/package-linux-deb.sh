#!/usr/bin/env bash
set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$(cd -- "$script_directory/.." && pwd)"
package_manifest="$workspace_root/crates/terminal_app/Cargo.toml"

usage() {
    cat <<'USAGE'
Usage: package-linux-deb.sh [--version VERSION] [--output PATH]

Build a Debian package from the existing release binary.
The release binary must already exist at target/release/terminal-app.
USAGE
}

version=""
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

if [[ -z "$output_path" ]]; then
    output_path="$workspace_root/target/package/zedterm-${version}-linux-amd64.deb"
elif [[ "$output_path" != /* ]]; then
    output_path="$(pwd)/$output_path"
fi

binary_path="$workspace_root/target/release/terminal-app"
if [[ ! -x "$binary_path" ]]; then
    echo "Release binary not found at $binary_path" >&2
    echo "Run: cargo build --locked --release -p terminal_app --bin terminal-app" >&2
    exit 1
fi

mkdir -p "$(dirname -- "$output_path")"
(
    cd "$workspace_root"
    cargo deb --locked --no-build -p terminal_app --output "$output_path"
)
desktop-file-validate "$workspace_root/crates/terminal_app/resources/dev.zed.ZedTerm.desktop"
dpkg-deb --info "$output_path"
dpkg-deb --contents "$output_path"

printf 'Created %s\n' "$output_path"
