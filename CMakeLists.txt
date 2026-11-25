# Pegasus bootstrap build. CMake + Ninja orchestrate; Cargo builds the Rust
# frontend crates; the C++ native backend (backend/) consumes verified PIR
# through the PIRB bridge and emits LLVM IR for the release pinned in
# toolchain.lock.
cmake_minimum_required(VERSION 3.25)
project(Pegasus VERSION 0.0.1 LANGUAGES NONE)

find_program(CARGO cargo REQUIRED)
find_package(Python3 3.11 REQUIRED COMPONENTS Interpreter)

set(PEGASUS_CARGO_PROFILE "release" CACHE STRING "Cargo profile for the Rust crates")
set(CARGO_ARGS --manifest-path ${CMAKE_SOURCE_DIR}/Cargo.toml --target-dir ${CMAKE_BINARY_DIR}/cargo --locked)

add_custom_target(pegasus_rust ALL
  COMMAND ${CARGO} build --profile ${PEGASUS_CARGO_PROFILE} ${CARGO_ARGS}
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR}
  COMMENT "cargo build (lexer, parser, sema, pir, pegasus driver, pgcorpus)"
  USES_TERMINAL)

enable_testing()
add_test(NAME rust_unit_tests
  COMMAND ${CARGO} test ${CARGO_ARGS} --workspace --exclude pegasus-parser
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
# Parser tests: rule 2a, nesting limit (P018), truncation, per-input watchdog
# fuzzing for token soup and byte soup, and a linear-time check. Optimized so
# the timing assertion is meaningful.
add_test(NAME parser_tests
  COMMAND ${CARGO} test --profile ${PEGASUS_CARGO_PROFILE} ${CARGO_ARGS} -p pegasus-parser
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
add_test(NAME corpus_format
  COMMAND ${Python3_EXECUTABLE} tools/corpus.py check
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
add_test(NAME grammar_ebnf_in_sync
  COMMAND ${Python3_EXECUTABLE} tools/extract_ebnf.py --check spec/grammar.md
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
add_test(NAME corpus_coverage
  COMMAND ${Python3_EXECUTABLE} tools/corpus.py coverage
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
add_test(NAME goldens_frozen_unchanged
  COMMAND ${Python3_EXECUTABLE} tools/goldens.py check
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
add_test(NAME golden_review_tool_tests
  COMMAND ${Python3_EXECUTABLE} tools/test_goldens.py
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})

option(PEGASUS_REQUIRE_ALL_GOLDEN_GROUPS_FROZEN
  "Gate 2: require the seven Phase 1 golden review groups to be frozen" OFF)
if(PEGASUS_REQUIRE_ALL_GOLDEN_GROUPS_FROZEN)
  add_test(NAME goldens_all_groups_frozen
    COMMAND ${Python3_EXECUTABLE} tools/goldens.py check --require-all-frozen
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
endif()
option(PEGASUS_REQUIRE_PARSER_GOLDENS_FROZEN
  "Parser milestone gate: require the parser golden review group to be frozen" OFF)
if(PEGASUS_REQUIRE_PARSER_GOLDENS_FROZEN)
  add_test(NAME goldens_parser_group_frozen
    COMMAND ${Python3_EXECUTABLE} tools/goldens.py check --require-phase2-frozen
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
endif()
# Differential oracle: Phase 0 integer semantics, Python model vs compiler + interpreter.
add_test(NAME integer_semantics_oracle
  COMMAND ${Python3_EXECUTABLE} tools/int_oracle.py ${CMAKE_BINARY_DIR}/cargo/${PEGASUS_CARGO_PROFILE}/pegasus${CMAKE_EXECUTABLE_SUFFIX} 300 7
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
# Lexer, newline filter and parser checks against every corpus case.
add_test(NAME corpus_conformance
  COMMAND ${CARGO} run --profile ${PEGASUS_CARGO_PROFILE} ${CARGO_ARGS} -q -p pegasus-corpus -- verify
  WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
set_tests_properties(corpus_conformance PROPERTIES
  ENVIRONMENT "PEGASUS_PYTHON=${Python3_EXECUTABLE}")

# Native backend (spec/native.md, spec/pir-bridge.md).
option(PEGASUS_BUILD_BACKEND "Build the C++ native backend and its tests" ON)
if(PEGASUS_BUILD_BACKEND)
  enable_language(C CXX)
  add_subdirectory(backend)
  # The LLVM pin (toolchain.lock) is enforced at configure time and by the
  # toolchain_pinned test. CI points PEGASUS_LLVM_ROOT at the prefix
  # tools/provision_llvm.py built from the pinned commit (strict, stamped);
  # a distribution clang reports no commit and is refused unless
  # PEGASUS_ALLOW_UNPINNED_LLVM is set, which keeps only the version check.
  set(PEGASUS_LLVM_ROOT "" CACHE PATH "Install prefix of the pinned LLVM (tools/provision_llvm.py)")
  option(PEGASUS_ALLOW_UNPINNED_LLVM
    "Accept a clang of the pinned version that reports no commit (the commit pin is NOT enforced)" OFF)
  set(PEGASUS_TOOLCHAIN_FLAGS "")
  if(PEGASUS_LLVM_ROOT)
    find_program(PEGASUS_CLANG NAMES clang PATHS ${PEGASUS_LLVM_ROOT}/bin NO_DEFAULT_PATH REQUIRED)
    list(APPEND PEGASUS_TOOLCHAIN_FLAGS --require-stamp)
  else()
    find_program(PEGASUS_CLANG NAMES clang-18 clang REQUIRED)
  endif()
  if(PEGASUS_ALLOW_UNPINNED_LLVM)
    list(APPEND PEGASUS_TOOLCHAIN_FLAGS --allow-unpinned)
  endif()
  execute_process(
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_SOURCE_DIR}/tools/check_toolchain.py ${PEGASUS_CLANG}
      ${PEGASUS_TOOLCHAIN_FLAGS}
    RESULT_VARIABLE PEGASUS_TOOLCHAIN_RC
    OUTPUT_VARIABLE PEGASUS_TOOLCHAIN_OUT
    ERROR_VARIABLE PEGASUS_TOOLCHAIN_OUT)
  string(STRIP "${PEGASUS_TOOLCHAIN_OUT}" PEGASUS_TOOLCHAIN_OUT)
  if(NOT PEGASUS_TOOLCHAIN_RC EQUAL 0)
    message(FATAL_ERROR "${PEGASUS_TOOLCHAIN_OUT}")
  elseif(PEGASUS_TOOLCHAIN_OUT MATCHES "NOT enforced")
    message(WARNING "${PEGASUS_TOOLCHAIN_OUT}")
  else()
    message(STATUS "${PEGASUS_TOOLCHAIN_OUT}")
  endif()
  set(PEGASUS_EXE ${CMAKE_BINARY_DIR}/cargo/${PEGASUS_CARGO_PROFILE}/pegasus${CMAKE_EXECUTABLE_SUFFIX})
  add_test(NAME toolchain_pinned
    COMMAND ${Python3_EXECUTABLE} tools/check_toolchain.py ${PEGASUS_CLANG} ${PEGASUS_TOOLCHAIN_FLAGS}
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
  add_test(NAME toolchain_check_selftest
    COMMAND ${Python3_EXECUTABLE} tools/test_check_toolchain.py
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
  # C ABI contract on real PIRB files produced by the frontend.
  add_test(NAME backend_abi_fixtures
    COMMAND ${PEGASUS_EXE} pirb ${CMAKE_SOURCE_DIR}/tests/native/places.peg ${CMAKE_BINARY_DIR}/places.pirb)
  add_test(NAME backend_abi_fixtures_unsupported
    COMMAND ${PEGASUS_EXE} pirb ${CMAKE_SOURCE_DIR}/tests/run/control.peg ${CMAKE_BINARY_DIR}/control.pirb)
  add_test(NAME backend_abi_fixtures_bool_eq
    COMMAND ${PEGASUS_EXE} pirb ${CMAKE_SOURCE_DIR}/backend/tests/fixtures/bool_eq.peg ${CMAKE_BINARY_DIR}/bool_eq.pirb)
  add_test(NAME backend_abi
    COMMAND abi_test ${CMAKE_BINARY_DIR}/places.pirb ${CMAKE_BINARY_DIR}/control.pirb
      ${CMAKE_BINARY_DIR}/bool_eq.pirb)
  set_tests_properties(backend_abi PROPERTIES
    DEPENDS "backend_abi_fixtures;backend_abi_fixtures_unsupported;backend_abi_fixtures_bool_eq")
  # Native executables vs the reference interpreter: corpus, integer
  # operators and generated programs (stdout, exit status, trap message).
  add_test(NAME native_differential
    COMMAND ${Python3_EXECUTABLE} tools/native_diff.py ${PEGASUS_EXE}
      $<TARGET_FILE:pegasus-native> $<TARGET_FILE:pegasus_rt>
      --clang ${PEGASUS_CLANG} --intops 300 --random 60 --seed 11
    WORKING_DIRECTORY ${CMAKE_SOURCE_DIR})
endif()
