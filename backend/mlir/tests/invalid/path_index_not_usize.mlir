// expected-error: array index must be usize
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%i: !pg.int<i32>) -> !pg.int<i32> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %s = "pg.slot"() : () -> !pg.slot<!pg.array<3 x !pg.int<i32>>>
    %v = "pg.load"(%s, %i) <{path = array<i64: -1>}> {pg.v = 1 : i64} : (!pg.slot<!pg.array<3 x !pg.int<i32>>>, !pg.int<i32>) -> !pg.int<i32>
    "pg.ret"(%v) : (!pg.int<i32>) -> ()
  }
}
