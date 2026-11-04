#!/bin/sh
# Installs the system libraries gpui needs to build and run on Linux, for
# both X11 and Wayland, on Debian and Ubuntu, and desktop-file-utils, which
# appimagetool needs to package the AppImage. CI and the release run this too,
# so this is the one list to keep up to date.
set -eu

sudo apt-get update
sudo apt-get install -y \
    desktop-file-utils \
    pkg-config \
    libasound2-dev \
    libfontconfig-dev \
    libfreetype-dev \
    libwayland-dev \
    libxkbcommon-dev \
    libxkbcommon-x11-dev \
    libx11-xcb-dev \
    libxcb1-dev
