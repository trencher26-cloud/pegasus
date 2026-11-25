// expected-error: store through a shared reference
module attributes {pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%r: !pg.ref<!pg.int<i32>>) -> !pg.tuple<()> {
    %c = "pg.const"() <{value = 1 : i128}> : () -> !pg.int<i32>
    "pg.store_ref"(%r, %c) <{path = array<i64>}> : (!pg.ref<!pg.int<i32>>, !pg.int<i32>) -> ()
    %u = "pg.tuple"() : () -> !pg.tuple<()>
    "pg.ret"(%u) : (!pg.tuple<()>) -> ()
  }
}
