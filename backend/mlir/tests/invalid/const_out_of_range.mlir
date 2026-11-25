// expected-error: integer constant 0x100 has bits outside `i8`
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"() -> !pg.int<i8> attributes {pg.name = "f", pg.block_params = [array<i64>]} {
    %c = "pg.const"() <{value = 256 : i128}> {pg.v = 0 : i64} : () -> !pg.int<i8>
    "pg.ret"(%c) : (!pg.int<i8>) -> ()
  }
}
