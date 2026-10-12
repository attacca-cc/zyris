# Zyris for NixOS: a released .deb, patched to run against nixpkgs' libraries.
#
# Repackaged rather than built from source: the build downloads ONNX Runtime and the web UI's
# packages, which Nix's sandbox forbids, and the .deb is the same binary every other Linux gets.
# `wrapGAppsHook3` is what makes it render: without the GSettings schemas on XDG_DATA_DIRS,
# WebKitGTK on Wayland lays the page out at a zoom of -1/96.
#
# **`lite ? false` picks which release asset is repackaged**, and it is the only difference: Lite
# is the same program built without the audio stack and without a window. A parameter rather than
# a second file because everything else here is what a Lite install needs too — the wrapping, the
# patched libraries, the tray's runtime dependency — since Lite is not a build without the toolkit
# (`README.md`), and the tray it does have is GTK. `flake.nix` publishes both as `zyris` and
# `zyris-lite`.
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
  lite ? false,
}:

let
  # The names the release assets carry: `Zyris-Lite_<version>_amd64.deb` against
  # `Zyris_<version>_amd64.deb`. The `.sig` files beside them are the updater's rather than
  # anything this derivation checks — `fetchurl` checks the hash below, which is the whole of what
  # a repackage needs, and it is what makes a Lite asset whose bytes changed fail here instead of
  # at run time.
  asset = if lite then "Zyris-Lite" else "Zyris";
  hash = if lite then
    "sha256-j+9scjMJEdfRoPPev8r4YIc7LJeM0Rt4lvQsdRS5Tbk="
  else
    "sha256-EUQcfdVaknBRTofNdmsdsr72UNZbdI8KH+P6LmZ9WP4=";
  description = if lite then
    "A desktop node for Attacca with no window and no audio stack: the tray, the tools and the console commands"
  else
    "A desktop node for Attacca: talk to your agent, and let it use this computer";
in
stdenv.mkDerivation (finalAttrs: {
  pname = if lite then "zyris-lite" else "zyris";
  version = "0.3.0";

  src = fetchurl {
    url = "https://github.com/attacca-cc/zyris/releases/download/v${finalAttrs.version}/${asset}_${finalAttrs.version}_amd64.deb";
    inherit hash;
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
    vulkan-loader
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
    description = description;
    homepage = "https://zyris.attacca.cc";
    license = lib.licenses.asl20;
    mainProgram = "zyris";
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
})
