// expected-error: char constant is not a Unicode scalar value
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"() -> !pg.char attributes {pg.name = "f", pg.block_params = [array<i64>]} {
    %c = "pg.const"() <{value = 55296 : i32}> {pg.v = 0 : i64} : () -> !pg.char
    "pg.ret"(%c) : (!pg.char) -> ()
  }
}
