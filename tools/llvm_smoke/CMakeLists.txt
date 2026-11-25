# Package-consumer smoke test for the pinned LLVM/MLIR toolchain package
# (tools/package_llvm.py smoke). It consumes the install prefix exactly as
# the Pegasus MLIR backend will: find_package(MLIR) through the installed
# CMake configuration, compile against the installed headers, link the
# installed libraries, and run a parse -> verify -> lower -> LLVM IR path.
cmake_minimum_required(VERSION 3.20)
project(pegasus_llvm_smoke CXX)

find_package(MLIR REQUIRED CONFIG)
message(STATUS "MLIR config: ${MLIR_DIR}")
message(STATUS "LLVM config: ${LLVM_DIR} (LLVM ${LLVM_PACKAGE_VERSION})")
if(NOT LLVM_PACKAGE_VERSION VERSION_EQUAL PEGASUS_EXPECT_LLVM_VERSION)
  message(FATAL_ERROR "LLVM ${LLVM_PACKAGE_VERSION} found, ${PEGASUS_EXPECT_LLVM_VERSION} expected")
endif()

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
include_directories(SYSTEM ${LLVM_INCLUDE_DIRS} ${MLIR_INCLUDE_DIRS})
separate_arguments(LLVM_DEFS NATIVE_COMMAND "${LLVM_DEFINITIONS}")
add_definitions(${LLVM_DEFS})
if(NOT LLVM_ENABLE_RTTI)
  if(MSVC)
    add_compile_options(/GR-)
  else()
    add_compile_options(-fno-rtti)
  endif()
endif()

add_executable(mlir_smoke mlir_smoke.cpp)
target_link_libraries(mlir_smoke PRIVATE
  MLIRIR MLIRParser MLIRPass MLIRFuncDialect MLIRArithDialect MLIRLLVMDialect
  MLIRArithToLLVM MLIRFuncToLLVM MLIRReconcileUnrealizedCasts
  MLIRBuiltinToLLVMIRTranslation MLIRLLVMToLLVMIRTranslation MLIRTargetLLVMIRExport)
