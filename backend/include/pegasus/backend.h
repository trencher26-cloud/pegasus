/* Pegasus native backend: the frozen C ABI of the Rust/C++ boundary.
 *
 * The frontend (Rust) lowers, checks and verifies a program and serializes
 * the verified PIR module as a PIRB file (spec/pir-bridge.md). The backend
 * (C++) consumes PIRB through this interface only. It does not re-check
 * types or ownership: PIRB files carry verified semantics. It does validate
 * the file's structure (every identifier in range) and fails closed.
 *
 * ABI version 1. Additions bump PEGASUS_BACKEND_ABI_MINOR; any change to an
 * existing declaration bumps PEGASUS_BACKEND_ABI_MAJOR.
 */
#ifndef PEGASUS_BACKEND_H
#define PEGASUS_BACKEND_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PEGASUS_BACKEND_ABI_MAJOR 1
#define PEGASUS_BACKEND_ABI_MINOR 0

/* The PIRB format version this backend reads (major must match exactly;
 * files with a higher minor version are rejected). */
#define PEGASUS_PIRB_VERSION_MAJOR 1
#define PEGASUS_PIRB_VERSION_MINOR 0

typedef enum pegasus_status {
  PEGASUS_OK = 0,
  /* The bytes are not a well-formed PIRB file of a supported version. */
  PEGASUS_ERR_FORMAT = 1,
  /* Well-formed, but uses PIR the native subset does not support yet
   * (spec/native.md). */
  PEGASUS_ERR_UNSUPPORTED = 2,
  /* Bad arguments (null pointers, no entry function). */
  PEGASUS_ERR_USAGE = 3
} pegasus_status;

/* "major.minor" of the ABI, plus the LLVM release the emitted IR targets. */
const char *pegasus_backend_version(void);

/* Translate a PIRB module to textual LLVM IR for the pinned LLVM release
 * (toolchain.lock). The IR defines `main`, which runs the module's entry
 * function and prints its result in the reference interpreter's format.
 *
 * On PEGASUS_OK, *out_ir receives a NUL-terminated string; otherwise
 * *out_error receives a NUL-terminated message. Either string must be
 * released with pegasus_backend_free. Neither output is written on a usage
 * error with a null output pointer. */
pegasus_status pegasus_backend_emit_llvm(const uint8_t *pirb, size_t len,
                                         char **out_ir, char **out_error);

void pegasus_backend_free(char *s);

#ifdef __cplusplus
}
#endif

#endif /* PEGASUS_BACKEND_H */
