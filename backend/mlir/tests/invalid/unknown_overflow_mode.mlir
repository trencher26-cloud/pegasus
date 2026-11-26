// expected-error: unknown overflow mode 'saturating'
module attributes {pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%a: !pg.int<i32>) -> !pg.int<i32> {
    %r = "pg.bin"(%a, %a) <{kind = "add", ov = "saturating"}> : (!pg.int<i32>, !pg.int<i32>) -> !pg.int<i32>
    "pg.ret"(%r) : (!pg.int<i32>) -> ()
  }
}
