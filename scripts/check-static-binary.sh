#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "Usage: check-static-binary.sh BINARY" >&2
    exit 1
fi

binary="$1"
# Capture readelf separately so an inspection failure cannot pass the check.
elf_info="$(LC_ALL=C readelf --wide --program-headers --dynamic "$binary")"
if printf '%s\n' "$elf_info" | grep -Eq 'INTERP|\(NEEDED\)'; then
    echo "$binary has a dynamic loader or shared-library dependencies" >&2
    printf '%s\n' "$elf_info" >&2
    exit 1
fi

"$binary" --version
