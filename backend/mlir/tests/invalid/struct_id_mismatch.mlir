// expected-error: id 1 differs from the result type '!pg.struct<0>'
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [{name = "A", field_names = ["x"], field_types = [!pg.int<i32>]}, {name = "B", field_names = ["x"], field_types = [!pg.int<i32>]}], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.int<i32>) -> !pg.struct<0> attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %s = "pg.struct"(%a) <{id = 1 : i64}> {pg.v = 1 : i64} : (!pg.int<i32>) -> !pg.struct<0>
    "pg.ret"(%s) : (!pg.struct<0>) -> ()
  }
}
