// expected-error: returned reference may point into this function's own storage
// Ownership: the function returns a borrow of its own slot, which dies when
// it returns. The dialect checks structure only; PIR's provenance rule (the
// Rust verifier) refuses it.
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"() -> !pg.ref<!pg.int<i32>> attributes {pg.name = "f", pg.block_params = [array<i64>], pg.named_slots} {
    %s = "pg.slot"() <{name = "local"}> : () -> !pg.slot<!pg.int<i32>>
    %c = "pg.const"() <{value = 1 : i128}> {pg.v = 0 : i64} : () -> !pg.int<i32>
    "pg.store"(%s, %c) <{path = array<i64>}> : (!pg.slot<!pg.int<i32>>, !pg.int<i32>) -> ()
    %r = "pg.borrow"(%s) <{path = array<i64>, exclusive = false}> {pg.v = 1 : i64} : (!pg.slot<!pg.int<i32>>) -> !pg.ref<!pg.int<i32>>
    "pg.ret"(%r) : (!pg.ref<!pg.int<i32>>) -> ()
  }
}
