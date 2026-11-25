// expected-error: struct id 3 has no definition in pg.structs
module attributes {pg.structs = [], pg.enums = []} {
  func.func @"f0.f"() -> !pg.int<i32> {
    %s = "pg.slot"() : () -> !pg.slot<!pg.struct<3>>
    %v = "pg.load"(%s) <{path = array<i64: 0>}> : (!pg.slot<!pg.struct<3>>) -> !pg.int<i32>
    "pg.ret"(%v) : (!pg.int<i32>) -> ()
  }
}
