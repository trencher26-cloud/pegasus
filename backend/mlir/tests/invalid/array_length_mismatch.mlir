// expected-error: has 1 elements, the array type has 3
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.int<i32>) -> !pg.array<3 x !pg.int<i32>> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %v = "pg.array"(%a) {pg.v = 1 : i64} : (!pg.int<i32>) -> !pg.array<3 x !pg.int<i32>>
    "pg.ret"(%v) : (!pg.array<3 x !pg.int<i32>>) -> ()
  }
}
