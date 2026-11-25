// expected-error: exclusive reborrow through a shared reference
module attributes {pg.structs = [], pg.enums = []} {
  func.func @"f0.f"(%r: !pg.ref<!pg.int<i32>>) -> !pg.ref<mut, !pg.int<i32>> {
    %m = "pg.borrow_ref"(%r) <{path = array<i64>, exclusive = true}> : (!pg.ref<!pg.int<i32>>) -> !pg.ref<mut, !pg.int<i32>>
    "pg.ret"(%m) : (!pg.ref<mut, !pg.int<i32>>) -> ()
  }
}
