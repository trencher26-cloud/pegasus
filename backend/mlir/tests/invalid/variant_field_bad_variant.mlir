// expected-error: variant 5 out of range (enum has 2)
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = [{name = "E", variant_names = ["A", "B"], variant_types = [[], [!pg.int<i32>]]}]} {
  func.func @"f0.f"(%e: !pg.enum<0>) -> !pg.int<i32> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %x = "pg.variant_field"(%e) <{variant = 5 : i64, index = 0 : i64}> {pg.v = 1 : i64} : (!pg.enum<0>) -> !pg.int<i32>
    "pg.ret"(%x) : (!pg.int<i32>) -> ()
  }
}
