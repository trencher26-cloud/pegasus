// expected-error: variant 2 out of range (enum has 2)
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = [{name = "E", variant_names = ["A", "B"], variant_types = [[], [!pg.int<i32>]]}]} {
  func.func @"f0.f"() -> !pg.enum<0> attributes {pg.name = "f", pg.block_params = [array<i64>]} {
    %e = "pg.variant"() <{id = 0 : i64, variant = 2 : i64}> {pg.v = 0 : i64} : () -> !pg.enum<0>
    "pg.ret"(%e) : (!pg.enum<0>) -> ()
  }
}
