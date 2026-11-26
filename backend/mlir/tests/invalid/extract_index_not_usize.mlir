// expected-error: array index must be usize
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.array<3 x !pg.int<i32>>, %i: !pg.int<u32>) -> !pg.int<i32> attributes {pg.name = "f", pg.block_params = [array<i64: 0, 1>]} {
    %v = "pg.extract_index"(%a, %i) {pg.v = 2 : i64} : (!pg.array<3 x !pg.int<i32>>, !pg.int<u32>) -> !pg.int<i32>
    "pg.ret"(%v) : (!pg.int<i32>) -> ()
  }
}
