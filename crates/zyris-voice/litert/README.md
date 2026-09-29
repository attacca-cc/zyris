# LiteRT C headers

The C API headers of [LiteRT](https://github.com/google-ai-edge/LiteRT) **2.2.0**, from its release
asset `litert_cc_sdk.zip`, under the Apache License 2.0. `build.rs` binds them with bindgen for the
NPU transcriber (`src/litert.rs`); the library itself, `libLiteRt.so`, comes from Maven
`com.google.ai.edge.litert:litert:2.2.0` and is loaded at run time.

`litert/build_common/build_config.h` is generated from LiteRT's `build_config.h.in` with both flags
off (`LITERT_BUILD_CONFIG_DISABLE_GPU 0`, `LITERT_BUILD_CONFIG_DISABLE_NPU 0`), as in the released
library. Update all of them together, from one release.
