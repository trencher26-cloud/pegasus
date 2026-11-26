// expected-error: index 2 out of range (2 fields)
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%t: !pg.tuple<(!pg.int<i32>, !pg.bool)>) -> !pg.int<i32> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %x = "pg.extract"(%t) <{index = 2 : i64}> {pg.v = 1 : i64} : (!pg.tuple<(!pg.int<i32>, !pg.bool)>) -> !pg.int<i32>
    "pg.ret"(%x) : (!pg.int<i32>) -> ()
  }
}
