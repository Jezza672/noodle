# VST3 and Audio Unit builds of Noodle, made from the CLAP with clap-wrapper's
# "CLAP first" mode. The plugin is Rust: `noodle-plugin` built as a static
# library with `--features wrapper` (see `build.sh`) is the implementation,
# and `entry.cpp` makes a `clap_entry` of its three functions for each format.
#
#   cmake -S packaging -B build/packaging -DNOODLE_PLUGIN_LIB=<libnoodle_plugin.a>
#   cmake --build build/packaging --target noodle_all
#
# The wrapper and the VST3 and AudioUnit SDKs are downloaded when it is
# configured. Output is under `build/packaging/` in `VST3/` (and `AUv2/` on
# macOS).

cmake_minimum_required(VERSION 3.21)
project(noodle_plugin VERSION 0.1.0 LANGUAGES C CXX)

if (APPLE)
  enable_language(OBJC OBJCXX)
  set(CMAKE_OSX_DEPLOYMENT_TARGET "11.0" CACHE STRING "")
endif()

set(NOODLE_PLUGIN_LIB "" CACHE FILEPATH
    "The static library: noodle-plugin built with --features wrapper")
if (NOT NOODLE_PLUGIN_LIB OR NOT EXISTS "${NOODLE_PLUGIN_LIB}")
  message(FATAL_ERROR "Set NOODLE_PLUGIN_LIB to the built static library (see build.sh)")
endif()

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)
set(CLAP_WRAPPER_DOWNLOAD_DEPENDENCIES TRUE CACHE BOOL "")
set(CLAP_WRAPPER_BUILD_AUV2 TRUE CACHE BOOL "")
set(CLAP_WRAPPER_BUILD_STANDALONE FALSE CACHE BOOL "")
set(CLAP_WRAPPER_BUILD_TESTS FALSE CACHE BOOL "")
set(CLAP_WRAPPER_DONT_ADD_TARGETS TRUE CACHE BOOL "")

include(FetchContent)
FetchContent_Declare(clap-wrapper
  GIT_REPOSITORY https://github.com/free-audio/clap-wrapper.git
  GIT_TAG v0.17.0
  GIT_SHALLOW TRUE)
FetchContent_MakeAvailable(clap-wrapper)

# `entry.cpp` includes the CLAP headers, which the wrapper downloads.
guarantee_clap()

# The Rust static library, and what it needs from the system.
add_library(noodle_impl STATIC IMPORTED GLOBAL)
target_link_libraries(noodle_impl INTERFACE clap)
set_target_properties(noodle_impl PROPERTIES IMPORTED_LOCATION "${NOODLE_PLUGIN_LIB}")
if (WIN32)
  target_link_libraries(noodle_impl INTERFACE
    ws2_32 userenv ntdll bcrypt advapi32 kernel32 user32 gdi32 shell32 ole32 oleaut32 dwmapi
    imm32 winmm propsys uxtheme comctl32 crypt32 secur32 ncrypt)
elseif (APPLE)
  target_link_libraries(noodle_impl INTERFACE
    "-framework Foundation" "-framework CoreFoundation" "-framework AppKit"
    "-framework CoreGraphics" "-framework CoreAudio" "-framework CoreMIDI"
    "-framework AudioToolbox" "-framework Metal" "-framework QuartzCore"
    "-framework Security" "-framework SystemConfiguration" "-framework CoreServices"
    "-framework IOKit" "-framework CoreText" "-framework CoreVideo"
    "-framework UniformTypeIdentifiers" iconv)
else()
  target_link_libraries(noodle_impl INTERFACE dl pthread m rt util asound)
endif()

set(NOODLE_FORMATS VST3)
if (APPLE)
  list(APPEND NOODLE_FORMATS AUV2)
endif()
# Always: the wrapper's CLAP target is what the AU configuration reads.
list(APPEND NOODLE_FORMATS CLAP)

make_clapfirst_plugins(
  TARGET_NAME noodle
  IMPL_TARGET noodle_impl
  OUTPUT_NAME "Noodle"
  ENTRY_SOURCE entry.cpp
  BUNDLE_IDENTIFIER "org.noodle.noodle"
  BUNDLE_VERSION "${PROJECT_VERSION}"
  PLUGIN_FORMATS ${NOODLE_FORMATS}
  ASSET_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/out"
)
