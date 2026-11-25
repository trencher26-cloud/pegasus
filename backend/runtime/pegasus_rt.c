/* Pegasus native runtime (spec/native.md §4): trap reporting and result
 * printing for executables produced by the native backend. Messages match
 * the reference interpreter's; exit status 101 on a trap, as `pegasus run`. */
#include <stdint.h>
#include <string.h>
#ifdef _WIN32
#include <fcntl.h>
#include <io.h>
#include <windows.h>
#else
#include <pthread.h>
#endif
#include <stdio.h>
#include <stdlib.h>

static void udec(uint64_t lo, uint64_t hi, char *out) {
  char tmp[48];
  int n = 0;
  if (!lo && !hi) tmp[n++] = '0';
  while (lo || hi) {
    uint32_t limb[4] = {(uint32_t)(hi >> 32), (uint32_t)hi, (uint32_t)(lo >> 32), (uint32_t)lo};
    uint64_t rem = 0;
    for (int i = 0; i < 4; ++i) {
      uint64_t cur = (rem << 32) | limb[i];
      limb[i] = (uint32_t)(cur / 10);
      rem = cur % 10;
    }
    hi = ((uint64_t)limb[0] << 32) | limb[1];
    lo = ((uint64_t)limb[2] << 32) | limb[3];
    tmp[n++] = (char)('0' + rem);
  }
  for (int i = 0; i < n; ++i) out[i] = tmp[n - 1 - i];
  out[n] = 0;
}

/* Print a 128-bit value given as halves; signed values are negative when
 * the top bit of hi is set. */
void pg_rt_print_int(uint64_t lo, uint64_t hi, int32_t is_signed) {
  char buf[48];
  if (is_signed && (hi >> 63)) {
    uint64_t nlo = ~lo + 1, nhi = ~hi + (lo == 0);
    udec(nlo, nhi, buf);
    fputc('-', stdout);
  } else {
    udec(lo, hi, buf);
  }
  fputs(buf, stdout);
}

void pg_rt_print_str(const char *s) { fputs(s, stdout); }

_Noreturn void pg_rt_trap(const char *msg) {
  fflush(stdout);
  fprintf(stderr, "trap: %s\n", msg);
  exit(101);
}

_Noreturn void pg_rt_trap_shift(uint64_t lo, uint64_t hi, const char *ty, uint32_t bits) {
  char buf[48];
  udec(lo, hi, buf);
  fflush(stdout);
  fprintf(stderr, "trap: attempt to shift by %s, not less than the bit width of `%s` (%u)\n", buf, ty, bits);
  exit(101);
}

_Noreturn void pg_rt_trap_index(uint64_t len, uint64_t lo, uint64_t hi) {
  char buf[48];
  udec(lo, hi, buf);
  fflush(stdout);
  fprintf(stderr, "trap: index out of bounds: the length is %llu but the index is %s\n", (unsigned long long)len, buf);
  exit(101);
}

/* Active Pegasus frames; every function checks it against the depth limit. */
uint64_t pg_rt_depth = 0;

/* Stack for the program thread: enough for the depth limit (100 000
 * frames) with room for large frames. Reserved, committed on demand. */
#define PG_RT_STACK ((size_t)4 << 30)

#ifdef _WIN32
static DWORD WINAPI pg_rt_thread(LPVOID f) {
  ((void (*)(void))f)();
  return 0;
}
void pg_rt_run(void (*program)(void)) {
  /* Byte-exact output, as the interpreter's: no "\n" -> "\r\n" translation. */
  _setmode(_fileno(stdout), _O_BINARY);
  _setmode(_fileno(stderr), _O_BINARY);
  HANDLE h = CreateThread(NULL, PG_RT_STACK, pg_rt_thread, (LPVOID)program, STACK_SIZE_PARAM_IS_A_RESERVATION, NULL);
  if (!h) { program(); return; }
  WaitForSingleObject(h, INFINITE);
  CloseHandle(h);
  fflush(stdout);
}
#else
static void *pg_rt_thread(void *f) {
  ((void (*)(void))f)();
  return NULL;
}
void pg_rt_run(void (*program)(void)) {
  pthread_attr_t a;
  pthread_t t;
  pthread_attr_init(&a);
  if (pthread_attr_setstacksize(&a, PG_RT_STACK) != 0 || pthread_create(&t, &a, pg_rt_thread, (void *)program) != 0) {
    pthread_attr_destroy(&a);
    program();
    fflush(stdout);
    return;
  }
  pthread_attr_destroy(&a);
  pthread_join(t, NULL);
  fflush(stdout);
}
#endif
