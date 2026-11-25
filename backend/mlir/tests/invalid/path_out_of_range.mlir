// expected-error: tuple field 2 out of range
module attributes {pg.structs = [], pg.enums = []} {
  func.func @"f0.f"() -> !pg.int<i32> {
    %s = "pg.slot"() : () -> !pg.slot<!pg.tuple<(!pg.int<i32>, !pg.int<i32>)>>
    %v = "pg.load"(%s) <{path = array<i64: 2>}> : (!pg.slot<!pg.tuple<(!pg.int<i32>, !pg.int<i32>)>>) -> !pg.int<i32>
    "pg.ret"(%v) : (!pg.int<i32>) -> ()
  }
}
