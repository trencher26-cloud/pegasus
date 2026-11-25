// expected-error: id 1 differs from the result type '!pg.enum<0>'
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = [{name = "E", variant_names = ["A"], variant_types = [[]]}, {name = "F", variant_names = ["A"], variant_types = [[]]}]} {
  func.func @"f0.f"() -> !pg.enum<0> attributes {pg.name = "f", pg.block_params = [array<i64>]} {
    %e = "pg.variant"() <{id = 1 : i64, variant = 0 : i64}> {pg.v = 0 : i64} : () -> !pg.enum<0>
    "pg.ret"(%e) : (!pg.enum<0>) -> ()
  }
}
