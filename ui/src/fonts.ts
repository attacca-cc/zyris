import pretendard from "pretendard/dist/web/variable/woff2/PretendardVariable.woff2?url";
import groteskLatin from "@fontsource-variable/space-grotesk/files/space-grotesk-latin-wght-normal.woff2?url";
import groteskLatinExt from "@fontsource-variable/space-grotesk/files/space-grotesk-latin-ext-wght-normal.woff2?url";

// **The fonts are registered here, through the FontFace API, and not as `@font-face` in CSS.**
// WebKitGTK rebuilds every `@font-face` in the document whenever any stylesheet changes, and
// with the 2 MB Pretendard among them that took ~480 ms. Radix adds and removes a <style> to lock
// scrolling each time a dropdown or dialog opens and closes, so every one of them froze the
// window for half a second each way. Faces added to `document.fonts` are not part of any
// stylesheet, and the same change measured 5 ms.
//
// Bundled rather than fetched: the window's CSP allows only 'self'.
const faces: [string, string, FontFaceDescriptors][] = [
  ["Pretendard Variable", pretendard, { weight: "45 920", display: "swap" }],
  [
    "Space Grotesk Variable",
    groteskLatin,
    {
      weight: "300 700",
      display: "swap",
      unicodeRange:
        "U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,U+0329,U+2000-206F,U+20AC,U+2122,U+2191,U+2193,U+2212,U+2215,U+FEFF,U+FFFD",
    },
  ],
  [
    "Space Grotesk Variable",
    groteskLatinExt,
    {
      weight: "300 700",
      display: "swap",
      unicodeRange:
        "U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+0304,U+0308,U+0329,U+1D00-1DBF,U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C0,U+2113,U+2C60-2C7F,U+A720-A7FF",
    },
  ],
];

export function loadFonts() {
  if (typeof FontFace === "undefined") return;
  for (const [family, url, descriptors] of faces) {
    const face = new FontFace(family, `url(${url}) format("woff2")`, descriptors);
    document.fonts.add(face);
    void face.load().catch(() => {});
  }
}
