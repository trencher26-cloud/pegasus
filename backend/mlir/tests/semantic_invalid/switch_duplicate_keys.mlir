// expected-error: duplicate switch case
// Two cases with the same key: well formed for the dialect, invalid PIR.
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.int<i32>) -> !pg.int<i32> attributes {pg.name = "f", pg.block_params = [array<i64: 0>, array<i64>, array<i64>]} {
    "pg.switch"(%a)[^bb1, ^bb2, ^bb2] <{cases = [1 : i128, 1 : i128]}> : (!pg.int<i32>) -> ()
  ^bb1:
    "pg.ret"(%a) : (!pg.int<i32>) -> ()
  ^bb2:
    "pg.ret"(%a) : (!pg.int<i32>) -> ()
  }
}
