// expected-error: type mismatch for bb argument #0 of successor #0
module attributes {pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.int<i32>) -> !pg.int<i64> {
    "pg.br"(%a)[^bb1] : (!pg.int<i32>) -> ()
  ^bb1(%x: !pg.int<i64>):
    "pg.ret"(%x) : (!pg.int<i64>) -> ()
  }
}
