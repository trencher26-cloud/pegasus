# Pegasus MLIR dialect, PIRB importer/exporter and pegasus-mlir (spec/mlir.md).
# Built against the pinned LLVM/MLIR package (PEGASUS_LLVM_ROOT) with the
# host compiler it was built with (GCC 13 / MSVC 19.52 or newer).
find_package(MLIR REQUIRED CONFIG PATHS ${PEGASUS_LLVM_ROOT}/lib/cmake/mlir NO_DEFAULT_PATH)
# The package is a Release build: on MSVC its libraries use the release
# runtime (/MD, _ITERATOR_DEBUG_LEVEL=0), and a Debug configuration (/MDd)
# cannot link with them.
if(MSVC AND NOT PEGASUS_MULTI_CONFIG AND CMAKE_BUILD_TYPE STREQUAL "Debug")
  message(FATAL_ERROR "The pinned LLVM/MLIR package is a Release build; with MSVC configure "
    "Pegasus with -DCMAKE_BUILD_TYPE=Release or RelWithDebInfo (or -DPEGASUS_BUILD_MLIR=OFF).")
endif()
message(STATUS "Pegasus MLIR: MLIR ${LLVM_PACKAGE_VERSION} from ${MLIR_DIR}")
list(APPEND CMAKE_MODULE_PATH "${MLIR_CMAKE_DIR}" "${LLVM_CMAKE_DIR}")
include(TableGen)
include(AddLLVM)
include(AddMLIR)

include_directories(SYSTEM ${LLVM_INCLUDE_DIRS} ${MLIR_INCLUDE_DIRS})
include_directories(${CMAKE_CURRENT_SOURCE_DIR}/include ${CMAKE_CURRENT_BINARY_DIR}/include)
add_subdirectory(include/pegasus/mlir)

separate_arguments(PEGASUS_LLVM_DEFS NATIVE_COMMAND "${LLVM_DEFINITIONS}")
set(PEGASUS_MLIR_FLAGS ${PEGASUS_LLVM_DEFS})
# LLVM and MLIR are built without RTTI; code deriving from their classes
# must match. Exceptions stay on (the PIRB reader reports errors by throwing).
if(MSVC)
  list(APPEND PEGASUS_MLIR_FLAGS /GR- /bigobj)
else()
  list(APPEND PEGASUS_MLIR_FLAGS -fno-rtti)
endif()

add_library(pegasus_mlir STATIC lib/PegasusDialect.cpp lib/Bridge.cpp ../src/pirb_write.cpp)
add_dependencies(pegasus_mlir PegasusDialectIncGen)
target_compile_features(pegasus_mlir PUBLIC cxx_std_17)
target_compile_options(pegasus_mlir PRIVATE ${PEGASUS_MLIR_FLAGS})
target_include_directories(pegasus_mlir PUBLIC include ${CMAKE_CURRENT_BINARY_DIR}/include ../src)
target_link_libraries(pegasus_mlir PUBLIC pegasus_backend
  MLIRIR MLIRParser MLIRFuncDialect MLIRControlFlowInterfaces MLIRSupport)

add_executable(pegasus-mlir tools/pegasus_mlir.cpp)
target_compile_options(pegasus-mlir PRIVATE ${PEGASUS_MLIR_FLAGS})
target_link_libraries(pegasus-mlir PRIVATE pegasus_mlir)

add_executable(pegasus-mlir-unit tests/unit.cpp)
target_compile_options(pegasus-mlir-unit PRIVATE ${PEGASUS_MLIR_FLAGS})
target_link_libraries(pegasus-mlir-unit PRIVATE pegasus_mlir)
