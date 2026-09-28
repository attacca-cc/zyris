# Whisper on a phone's GPU: where ggml-vulkan finds Vulkan when it is cross-compiled for Android.
# Handed to whisper.cpp's CMake as CMAKE_PROJECT_INCLUDE_BEFORE by `.github/workflows/mobile.yml`.
#
# FindVulkan cannot find these by itself under the NDK: the loader and glslc live in the NDK but
# outside the paths CMake searches, and the NDK carries only the C headers, while ggml-vulkan is
# C++ and needs vulkan.hpp, so the headers come from Khronos (ZYRIS_VULKAN_HEADERS). The loader
# stub is the one for API 26, the app's minSdk.
set(Vulkan_INCLUDE_DIR "$ENV{ZYRIS_VULKAN_HEADERS}/include" CACHE PATH "" FORCE)
set(Vulkan_LIBRARY "$ENV{NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/26/libvulkan.so" CACHE FILEPATH "" FORCE)
set(Vulkan_GLSLC_EXECUTABLE "$ENV{NDK_HOME}/shader-tools/linux-x86_64/glslc" CACHE FILEPATH "" FORCE)
