# Pegasus native backend (C++17): PIRB reader, LLVM IR emitter, C ABI,
# the pegasus-native tool and the C runtime. The emitted IR targets the LLVM
# release pinned in toolchain.lock; clang from that release compiles it.
add_library(pegasus_backend STATIC
  src/pirb.cpp
  src/emit_llvm.cpp
  src/backend.cpp)
target_include_directories(pegasus_backend PUBLIC include)
target_compile_features(pegasus_backend PUBLIC cxx_std_17)
if(NOT MSVC)
  target_compile_options(pegasus_backend PRIVATE -Wall -Wextra -Werror)
endif()

add_executable(pegasus-native tools/pegasus_native.cpp)
target_link_libraries(pegasus-native PRIVATE pegasus_backend)

add_library(pegasus_rt STATIC runtime/pegasus_rt.c)
# clang targeting MSVC links native programs against the static C runtime
# (libcmt) by default, so the runtime library must use it too: built with
# CMake's default DLL runtime (/MD) it references __imp_* symbols that
# libcmt does not define (LNK1120 on every program in the first Windows
# CI run).
set_target_properties(pegasus_rt PROPERTIES C_STANDARD 11 MSVC_RUNTIME_LIBRARY "MultiThreaded")

add_executable(abi_test tests/abi_test.cpp)
target_link_libraries(abi_test PRIVATE pegasus_backend)
