// expected-error: cast is not a lossless conversion
// pg.cast has no verifier: losslessness is PIR's rule (i32 -> i8 is lossy).
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.int<i32>) -> !pg.int<i8> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %r = "pg.cast"(%a) {pg.v = 1 : i64} : (!pg.int<i32>) -> !pg.int<i8>
    "pg.ret"(%r) : (!pg.int<i8>) -> ()
  }
}
