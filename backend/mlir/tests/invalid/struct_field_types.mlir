// expected-error: fields #0 has type '!pg.int<i64>', expected '!pg.int<i32>'
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [{name = "A", field_names = ["x"], field_types = [!pg.int<i32>]}], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.int<i64>) -> !pg.struct<0> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %s = "pg.struct"(%a) <{id = 0 : i64}> {pg.v = 1 : i64} : (!pg.int<i64>) -> !pg.struct<0>
    "pg.ret"(%s) : (!pg.struct<0>) -> ()
  }
}
