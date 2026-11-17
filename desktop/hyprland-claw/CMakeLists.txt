cmake_minimum_required(VERSION 3.21)

project(claw-compositor-adapter LANGUAGES CXX)

if(NOT CMAKE_SYSTEM_NAME STREQUAL "Linux")
    message(FATAL_ERROR "Claw compositor authority requires Linux")
endif()

set(CLAW_COMPOSITOR_LIBRARY "" CACHE FILEPATH "Built OS-private Rust authority static library")
if(NOT EXISTS "${CLAW_COMPOSITOR_LIBRARY}")
    message(FATAL_ERROR "Build claw-compositor and provide CLAW_COMPOSITOR_LIBRARY")
endif()

find_package(PkgConfig REQUIRED)
pkg_check_modules(CLAW_WAYLAND REQUIRED IMPORTED_TARGET wayland-server>=1.22.91)
find_package(Threads REQUIRED)

add_library(claw_compositor_rust STATIC IMPORTED GLOBAL)
set_target_properties(claw_compositor_rust PROPERTIES
    IMPORTED_LOCATION "${CLAW_COMPOSITOR_LIBRARY}"
)

add_library(claw_compositor_adapter STATIC Claw.cpp)
set_target_properties(claw_compositor_adapter PROPERTIES POSITION_INDEPENDENT_CODE ON)
target_compile_features(claw_compositor_adapter PUBLIC cxx_std_20)
target_include_directories(claw_compositor_adapter PUBLIC "${CMAKE_CURRENT_SOURCE_DIR}")
target_compile_options(claw_compositor_adapter PRIVATE -Wall -Wextra -Wpedantic)
target_link_libraries(claw_compositor_adapter PUBLIC
    claw_compositor_rust PkgConfig::CLAW_WAYLAND Threads::Threads ${CMAKE_DL_LIBS} m
)

if(PROJECT_IS_TOP_LEVEL)
    enable_testing()
    add_executable(claw_compositor_abi test/abi.cpp)
    target_link_libraries(claw_compositor_abi PRIVATE claw_compositor_adapter)
    add_test(NAME native-authority-refusal COMMAND claw_compositor_abi)
endif()
