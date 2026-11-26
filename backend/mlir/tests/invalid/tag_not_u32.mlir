// expected-error: a tag is a u32
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = [{name = "E", variant_names = ["A"], variant_types = [[]]}]} {
  func.func @"f0.f"(%e: !pg.enum<0>) -> !pg.int<i64> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %t = "pg.tag"(%e) {pg.v = 1 : i64} : (!pg.enum<0>) -> !pg.int<i64>
    "pg.ret"(%t) : (!pg.int<i64>) -> ()
  }
}
