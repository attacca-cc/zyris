# Zyris for NixOS: the released .deb, patched to run against nixpkgs' libraries.
#
# Repackaged rather than built from source: the build downloads ONNX Runtime and the web UI's
# packages, which Nix's sandbox forbids, and the .deb is the same binary every other Linux gets.
# `wrapGAppsHook3` is what makes it render: without the GSettings schemas on XDG_DATA_DIRS,
# WebKitGTK on Wayland lays the page out at a zoom of -1/96.
{
  lib,
  stdenv,
  fetchurl,
  dpkg,
  autoPatchelfHook,
  wrapGAppsHook3,
  webkitgtk_4_1,
  gtk3,
  libsoup_3,
  glib,
  glib-networking,
  cairo,
  pango,
  gdk-pixbuf,
  atk,
  harfbuzz,
  openssl,
  alsa-lib,
  pipewire,
  libgbm,
  libayatana-appindicator,
  libxkbcommon,
  dbus,
  xorg,
  wayland,
  libGL,
  vulkan-loader,
}:

stdenv.mkDerivation (finalAttrs: {
  pname = "zyris";
  version = "0.1.1";

  src = fetchurl {
    url = "https://github.com/attacca-cc/zyris/releases/download/v${finalAttrs.version}/Zyris_${finalAttrs.version}_amd64.deb";
    hash = "sha256-IO0A6EF00C/s+SQo5o4ph3vidzjoYQwMXnFsxOzVPV4=";
  };

  nativeBuildInputs = [
    dpkg
    autoPatchelfHook
    wrapGAppsHook3
  ];

  buildInputs = [
    webkitgtk_4_1
    gtk3
    libsoup_3
    glib
    glib-networking
    cairo
    pango
    gdk-pixbuf
    atk
    harfbuzz
    openssl
    alsa-lib
    pipewire
    libgbm
    libxkbcommon
    dbus
    xorg.libxcb
    xorg.libX11
    wayland
  ];

  # Loaded with dlopen, so autoPatchelf cannot see them in the binary: the tray, and the
  # graphics the webview and the GPU backends reach for.
  runtimeDependencies = [
    libayatana-appindicator
    libGL
    vulkan-loader
  ];

  unpackPhase = ''
    runHook preUnpack
    dpkg-deb -x $src .
    runHook postUnpack
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp -r usr/* $out/
    runHook postInstall
  '';

  meta = {
    description = "A desktop node for Attacca: talk to your agent, and let it use this computer";
    homepage = "https://zyris.attacca.cc";
    license = lib.licenses.asl20;
    mainProgram = "zyris";
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
})
