#!/bin/sh
# Builds Wine 11.0 for musl in the `zooos-build` WSL Alpine environment
# (tools/alpine/README.md, docs/linux10-design.md).
#
#   wsl -d zooos-build -u root -- sh tools/wine/build-wine.sh <path to wine-11.0.tar.xz>
#
# A lean 64-bit build: no X11, Wayland, Vulkan, audio, printing, scanners,
# cameras, smart cards, Kerberos, TLS or 32-bit support yet; ZooOS has none
# of those underneath. FreeType (fonts) is kept. Build packages, as root:
#   apk add build-base bison flex-dev mingw-w64-gcc freetype-dev perl xz linux-headers
# The build happens on Linux's own disk (fast); the result is installed into
# $WORK/install and packed as $WORK/wine-11.0-zooos.tar.xz.
set -eu
TARBALL=${1:?usage: build-wine.sh <wine-11.0.tar.xz>}
WORK=/root/wine-build
mkdir -p "$WORK"
cd "$WORK"
[ -d wine-11.0 ] || tar xf "$TARBALL"
mkdir -p build64
cd build64
if [ ! -f Makefile ]; then
    ../wine-11.0/configure \
        --prefix=/usr --libdir=/usr/lib --sysconfdir=/etc --localstatedir=/var \
        --enable-win64 --enable-archs=x86_64 --with-mingw --disable-tests \
        --without-x --without-wayland --without-vulkan --without-opengl \
        --without-alsa --without-pulse --without-oss --without-cups \
        --without-dbus --without-gstreamer --without-sane --without-gphoto \
        --without-usb --without-v4l2 --without-pcap --without-krb5 \
        --without-gssapi --without-opencl --without-pcsclite --without-sdl \
        --without-udev --without-capi --without-netapi --without-gnutls \
        --without-unwind --without-ffmpeg
fi
make -j"$(nproc)"
rm -rf "$WORK/install"
make install DESTDIR="$WORK/install"
cd "$WORK/install"
tar cJf "$WORK/wine-11.0-zooos.tar.xz" .
du -sh "$WORK/install" "$WORK/wine-11.0-zooos.tar.xz"
