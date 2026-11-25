//! Robustness harness: NUL-separated sources; every program sema accepts is
//! lowered, verified and run with small limits. Reports panics and verifier
//! failures (traps are fine: mutated programs may divide by zero). Every
//! verified module also round-trips through the PIRB bridge byte-exactly.
use pegasus_pir::interp::{Interp, Limits};
use pegasus_sema::source::MemLoader;
use std::io::Read;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let mut data = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    std::panic::set_hook(Box::new(|_| {}));
    let (mut n, mut accepted, mut panics, mut ice, mut ran) = (0, 0, 0, 0, 0);
    for prog in data.split(|&b| b == 0) {
        n += 1;
        let src = String::from_utf8_lossy(prog).to_string();
        let s2 = src.clone();
        let r = std::panic::catch_unwind(move || {
            let loader = MemLoader::new(&[("main.peg", &s2)]);
            pegasus_sema::check_package_with(&loader, "main.peg", |s| {
                let c = pegasus_pir::compile(s)?;
                if !c.verify_errors.is_empty() {
                    return Some((true, false));
                }
                let entry = c
                    .main
                    .filter(|m| c.module.funcs[*m as usize].params.is_empty());
                let b = pegasus_pir::bridge::encode(&c.module, entry).ok()?;
                let d = pegasus_pir::bridge::decode(&b).expect("PIRB round trip");
                assert_eq!(
                    pegasus_pir::bridge::encode(&d.module, d.entry).unwrap(),
                    b,
                    "PIRB not lossless"
                );
                let mut did_run = false;
                if let Some(m) = c.main {
                    if c.errors.is_empty() && c.module.funcs[m as usize].params.is_empty() {
                        let mut it = Interp::new(
                            &c.module,
                            Limits {
                                max_steps: 200_000,
                                max_depth: 2_000,
                            },
                        );
                        let _ = it.call(m, vec![]);
                        did_run = true;
                    }
                }
                Some((false, did_run))
            })
            .1
        });
        match r {
            Err(_) => {
                panics += 1;
                std::fs::write(format!("/tmp/pir_panic_{panics}.peg"), &src).unwrap();
            }
            Ok(Some((bad, did_run))) => {
                accepted += 1;
                if bad {
                    ice += 1;
                    std::fs::write(format!("/tmp/pir_ice_{ice}.peg"), &src).unwrap();
                }
                if did_run {
                    ran += 1;
                }
            }
            Ok(None) => {}
        }
    }
    println!("{n} programs, {accepted} accepted by sema+PURM, {ran} run, {ice} verifier failures or soundness traps, {panics} panics");
}
