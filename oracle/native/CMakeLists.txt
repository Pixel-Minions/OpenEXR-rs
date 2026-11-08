# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
#
# The source-built Core harness (PLAN.md O1): upstream OpenEXRCore from the pinned submodule,
# with the official wheel's dependency settings (as scripts/upstream-build.*), and
# core_harness.c linked against it. Built by scripts/oracle-native-build.{cmd,sh}.

cmake_minimum_required(VERSION 3.25)
project(openexr_core_harness C CXX)

set(OPENEXR_SRC "${CMAKE_CURRENT_SOURCE_DIR}/../../upstream/openexr" CACHE PATH "upstream OpenEXR")

# the wheel's settings (upstream pyproject.toml [tool.scikit-build.cmake.define])
set(BUILD_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(CMAKE_POSITION_INDEPENDENT_CODE ON CACHE BOOL "" FORCE)
set(OPENEXR_INSTALL OFF CACHE BOOL "" FORCE)
set(OPENEXR_INSTALL_PKG_CONFIG OFF CACHE BOOL "" FORCE)
set(OPENEXR_BUILD_TOOLS OFF CACHE BOOL "" FORCE)
set(OPENEXR_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(OPENEXR_BUILD_PYTHON OFF CACHE BOOL "" FORCE)
set(OPENEXR_FORCE_INTERNAL_DEFLATE ON CACHE BOOL "" FORCE)
set(OPENEXR_FORCE_INTERNAL_IMATH ON CACHE BOOL "" FORCE)
set(OPENEXR_FORCE_INTERNAL_OPENJPH ON CACHE BOOL "" FORCE)
set(OPENEXR_FORCE_INTERNAL_ZSTD ON CACHE BOOL "" FORCE)
set(BUILD_TESTING OFF CACHE BOOL "" FORCE)

add_subdirectory("${OPENEXR_SRC}" openexr EXCLUDE_FROM_ALL)

add_executable(core_harness core_harness.c)
target_link_libraries(core_harness PRIVATE OpenEXR::OpenEXRCore)
