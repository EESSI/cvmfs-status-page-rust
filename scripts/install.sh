#!/bin/sh
set -eu

repo="EESSI/cvmfs-status-page-rust"
binary_name="cvmfs-status-page-rust"
tag=""
version=""
install_dir=""

usage() {
    cat <<EOF
Usage:
  install.sh --tag vX.Y.Z --install-dir DIR
  install.sh --version X.Y.Z --install-dir DIR

Options:
  --tag TAG          Release tag to install, for example v0.0.1.
  --version VERSION  Release version to install, for example 0.0.1.
  --install-dir DIR  Directory where ${binary_name} should be installed.
  --repo OWNER/REPO  GitHub repository to install from. Defaults to ${repo}.
  -h, --help         Show this help.
EOF
}

die() {
    echo "install.sh: $*" >&2
    exit 1
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --tag)
            [ "$#" -ge 2 ] || die "--tag requires a value"
            tag="$2"
            shift 2
            ;;
        --version)
            [ "$#" -ge 2 ] || die "--version requires a value"
            version="$2"
            shift 2
            ;;
        --install-dir)
            [ "$#" -ge 2 ] || die "--install-dir requires a value"
            install_dir="$2"
            shift 2
            ;;
        --repo)
            [ "$#" -ge 2 ] || die "--repo requires a value"
            repo="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown argument: $1"
            ;;
    esac
done

[ -n "$install_dir" ] || die "--install-dir is required"

if [ -n "$tag" ] && [ -n "$version" ]; then
    die "use either --tag or --version, not both"
fi

if [ -n "$version" ]; then
    case "$version" in
        v*) die "--version should not include the leading v; use --tag for tags" ;;
    esac
    tag="v${version}"
elif [ -n "$tag" ]; then
    case "$tag" in
        v*) version="${tag#v}" ;;
        *) die "--tag must start with v, for example v0.0.1" ;;
    esac
else
    die "one of --tag or --version is required"
fi

need_cmd uname

os="$(uname -s)"
[ "$os" = "Linux" ] || die "unsupported operating system: $os"

need_cmd curl
need_cmd tar
need_cmd sha256sum
need_cmd mktemp

arch="$(uname -m)"
case "$arch" in
    x86_64|amd64)
        target="x86_64-unknown-linux-gnu"
        ;;
    aarch64|arm64)
        target="aarch64-unknown-linux-gnu"
        ;;
    *)
        die "unsupported architecture: $arch"
        ;;
esac

package="${binary_name}-${version}-${target}"
archive="${package}.tar.gz"
checksum="${archive}.sha256"
base_url="https://github.com/${repo}/releases/download/${tag}"

tmpdir="$(mktemp -d)"
staged=""
cleanup() {
    if [ -n "$staged" ] && [ -e "$staged" ]; then
        rm -f "$staged"
    fi
    rm -rf "$tmpdir"
}
trap cleanup EXIT INT HUP TERM

echo "Downloading ${repo} ${tag} for ${target}"
curl -fsSL "${base_url}/${archive}" -o "${tmpdir}/${archive}"
curl -fsSL "${base_url}/${checksum}" -o "${tmpdir}/${checksum}"

(
    cd "$tmpdir"
    sha256sum -c "$checksum"
)

tar -xzf "${tmpdir}/${archive}" -C "$tmpdir"
[ -x "${tmpdir}/${package}/${binary_name}" ] || die "archive does not contain executable ${binary_name}"

mkdir -p "$install_dir"
[ -d "$install_dir" ] || die "install directory is not a directory: $install_dir"
[ -w "$install_dir" ] || die "install directory is not writable: $install_dir"

destination="${install_dir%/}/${binary_name}"
staged="$(mktemp "${install_dir%/}/.${binary_name}.tmp.XXXXXX")"
cp "${tmpdir}/${package}/${binary_name}" "$staged"
chmod 0755 "$staged"

staged_version="$("$staged" --version 2>/dev/null || true)"
case "$staged_version" in
    "${binary_name} ${version}") ;;
    *)
        die "downloaded binary reported unexpected version: ${staged_version}"
        ;;
esac

mv "$staged" "$destination"

echo "Installed ${binary_name} ${version} to ${destination}"
