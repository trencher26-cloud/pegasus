// expected-error: Add is not defined on bool
// Structurally valid (result type equals the left operand's), but PIR has no
// arithmetic on bool.
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.bool) -> !pg.bool attributes {pg.name = "f", pg.block_params = [array<i64: 0>]} {
    %r = "pg.bin"(%a, %a) <{kind = "add", ov = "trap"}> {pg.v = 1 : i64} : (!pg.bool, !pg.bool) -> !pg.bool
    "pg.ret"(%r) : (!pg.bool) -> ()
  }
}
