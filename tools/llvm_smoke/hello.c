/* Compiled and run by the package's own clang (tools/package_llvm.py smoke). */
#include <stdio.h>
int main(void) {
  printf("pegasus llvm package smoke: clang %d.%d.%d\n", __clang_major__, __clang_minor__, __clang_patchlevel__);
  return 0;
}
