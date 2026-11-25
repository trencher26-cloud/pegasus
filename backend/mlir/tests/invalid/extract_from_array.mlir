// expected-error: extract from something other than a tuple or struct
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.array<3 x !pg.int<i32>>) -> !pg.int<i32> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %v = "pg.extract"(%a) <{index = 1 : i64}> {pg.v = 1 : i64} : (!pg.array<3 x !pg.int<i32>>) -> !pg.int<i32>
    "pg.ret"(%v) : (!pg.int<i32>) -> ()
  }
}
