// expected-error: array index must be usize
module attributes {pg.pirb_minor = 0 : i64, pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%r: !pg.ref<mut, !pg.array<3 x !pg.int<i32>>>, %i: !pg.int<u64>, %x: !pg.int<i32>) -> !pg.tuple<()> attributes {pg.name = "f", pg.block_params = [array<i64: 0, 1, 2>]} {
    "pg.store_ref"(%r, %x, %i) <{path = array<i64: -1>}> : (!pg.ref<mut, !pg.array<3 x !pg.int<i32>>>, !pg.int<i32>, !pg.int<u64>) -> ()
    %u = "pg.tuple"() {pg.v = 3 : i64} : () -> !pg.tuple<()>
    "pg.ret"(%u) : (!pg.tuple<()>) -> ()
  }
}
