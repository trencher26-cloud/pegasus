// expected-error: has bits outside `u32`
// -1 as i128 is 128 one bits; a u32 constant is its 32-bit pattern (0xffffffff).
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"() -> !pg.int<u32> attributes {pg.name = "f", pg.block_params = [array<i64>]} {
    %c = "pg.const"() <{value = -1 : i128}> {pg.v = 0 : i64} : () -> !pg.int<u32>
    "pg.ret"(%c) : (!pg.int<u32>) -> ()
  }
}
