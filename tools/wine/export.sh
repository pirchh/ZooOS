#!/bin/sh
# Stages the Wine build (build-wine.sh) for ZooOS's `wine` image
# (docs/linux10-design.md) and writes it as a plain ustar archive:
#
#   wsl -d zooos-build -u root -- sh tools/wine/export.sh <output .tar>
#
# Debug information is stripped, developer-only files (headers, man pages,
# import libraries) are left out, symbolic links become copies, and the
# libraries Wine loads besides musl are added under lib/: libgcc_s, and
# FreeType with its own dependencies. Needs: apk add tar
set -eu
OUT=${1:?usage: export.sh <output .tar>}
W=/root/wine-build
STAGE=$W/stage
rm -rf "$STAGE"
mkdir -p "$STAGE/lib"
cp -a "$W/install/usr" "$STAGE/"
cd "$STAGE"
rm -rf usr/include usr/share/man usr/share/applications
find usr/lib/wine -name '*.a' -delete
find usr/lib/wine/x86_64-windows -type f -exec x86_64-w64-mingw32-strip --strip-unneeded {} + 2>/dev/null || true
find usr/bin usr/lib/wine/x86_64-unix -type f -exec strip --strip-unneeded {} + 2>/dev/null || true
for l in libgcc_s.so.1 libfreetype.so.6 libz.so.1 libbz2.so.1 libpng16.so.16 \
         libbrotlidec.so.1 libbrotlicommon.so.1; do
    src=$(ls /usr/lib/$l /lib/$l 2>/dev/null | head -1)
    [ -n "$src" ] || { echo "missing $l" >&2; exit 1; }
    cp -L "$src" lib/
done
find usr lib \( -type f -o -type l \) | sort > "$W/files.list"
tar --format=ustar --dereference --hard-dereference --owner=0 --group=0 --mtime=@0 -cf "$OUT" -T "$W/files.list"
wc -l < "$W/files.list"
du -h "$OUT"
