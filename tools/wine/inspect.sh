#!/bin/sh
# Lists what a Wine build's Unix side needs (run in the zooos-build WSL
# environment after build-wine.sh): each ELF file's DT_NEEDED libraries.
set -eu
cd /root/wine-build/stripped
for f in usr/bin/wine usr/bin/wineserver usr/lib/wine/x86_64-unix/*.so; do
    need=$(readelf -d "$f" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | tr '\n' ' ')
    echo "$f: $need"
done | sort | awk -F': ' '{print $2}' | tr ' ' '\n' | sort | uniq -c | sort -rn
