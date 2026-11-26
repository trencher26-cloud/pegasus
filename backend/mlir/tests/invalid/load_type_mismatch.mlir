// expected-error: result has type '!pg.int<i64>', the place has type '!pg.int<i32>'
module attributes {pg.structs = [], pg.enums = []} {
  func.func @"f0.f"() -> !pg.int<i64> {
    %s = "pg.slot"() : () -> !pg.slot<!pg.int<i32>>
    %v = "pg.load"(%s) <{path = array<i64>}> : (!pg.slot<!pg.int<i32>>) -> !pg.int<i64>
    "pg.ret"(%v) : (!pg.int<i64>) -> ()
  }
}
