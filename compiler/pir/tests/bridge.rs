//! The PIR bridge encoding (spec/pir-bridge.md) is lossless on every corpus
//! module, refuses unverified modules, and its reader fails cleanly (never
//! panics) on truncated or corrupted input.

use pegasus_pir::bridge::{crc32, decode, encode, FLAG_VERIFIED, VERSION_MAJOR, VERSION_MINOR};

/// Recompute the trailer after editing a file in a test.
fn reseal(b: &mut [u8]) {
    let n = b.len() - 4;
    let c = crc32(&b[..n]);
    b[n..].copy_from_slice(&c.to_le_bytes());
}
use pegasus_pir::ir::*;
use pegasus_sema::source::FsLoader;
use std::path::Path;

fn corpus() -> Vec<(String, Module, Option<FuncId>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests");
    let mut out = Vec::new();
    for dir in ["run", "sema/accept", "native"] {
        let d = root.join(dir);
        let mut files: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        for f in files
            .into_iter()
            .filter(|f| f.extension().is_some_and(|e| e == "peg"))
        {
            let name = f.file_name().unwrap().to_str().unwrap().to_string();
            let loader = FsLoader { root: d.clone() };
            let (_, c) = pegasus_sema::check_package_with(&loader, &name, |s| {
                pegasus_pir::compile(s).map(|c| (c.module, c.main))
            });
            let (m, main) = c.expect("corpus program must compile");
            out.push((format!("{dir}/{name}"), m, main));
        }
    }
    out
}

#[test]
fn round_trip_is_lossless() {
    let mut n = 0;
    for (name, m, main) in corpus() {
        let bytes = encode(&m, main).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let d = decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(d.flags, FLAG_VERIFIED);
        assert_eq!(d.minor, VERSION_MINOR);
        assert_eq!(d.entry, main, "{name}");
        // Re-encoding is byte-identical (also covers NaN constants, which
        // `PartialEq` cannot compare), and the module compares equal.
        assert_eq!(encode(&d.module, d.entry).unwrap(), bytes, "{name}");
        assert_eq!(d.module.to_string(), m.to_string(), "{name}");
        n += 1;
    }
    assert!(n >= 15, "only {n} corpus modules");
}

#[test]
fn header_layout() {
    let (_, m, main) = corpus().into_iter().next().unwrap();
    let b = encode(&m, main).unwrap();
    assert_eq!(&b[..4], b"PIRB");
    assert_eq!(u16::from_le_bytes([b[4], b[5]]), VERSION_MAJOR);
    assert_eq!(u16::from_le_bytes([b[6], b[7]]), VERSION_MINOR);
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926, "CRC-32/IEEE check value");
    let mut wrong = b.clone();
    wrong[4] = 2;
    assert!(decode(&wrong)
        .unwrap_err()
        .contains("unsupported PIRB version"));
    let mut corrupt = b.clone();
    corrupt[10] ^= 1;
    assert!(decode(&corrupt).unwrap_err().contains("checksum"));
    reseal(&mut wrong);
    assert!(decode(&wrong)
        .unwrap_err()
        .contains("unsupported PIRB version"));
    let mut newer = b.clone();
    newer[6] = 1;
    reseal(&mut newer);
    assert!(decode(&newer)
        .unwrap_err()
        .contains("unsupported PIRB version"));
    let mut unverified = b.clone();
    unverified[8] = 0;
    reseal(&mut unverified);
    assert!(decode(&unverified)
        .unwrap_err()
        .contains("not marked verified"));
    let mut extra = b.clone();
    let n = extra.len() - 4;
    extra.insert(n, 0);
    reseal(&mut extra);
    assert!(decode(&extra).unwrap_err().contains("trailing"));
}

/// Replace the header's `flags` field (LEB128 at byte 8) and reseal.
fn with_flags(b: &[u8], flags: u64) -> Vec<u8> {
    assert_eq!(b[8], 1, "encoder writes flags = 1 as one byte");
    let mut out = b[..8].to_vec();
    let mut x = flags;
    loop {
        let byte = (x & 0x7f) as u8;
        x >>= 7;
        if x == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out.extend_from_slice(&b[9..]);
    reseal(&mut out);
    out
}

/// Finding 2 of the PIR0.4 review: reserved flag bits must be zero.
#[test]
fn every_reserved_flag_bit_is_refused() {
    let (_, m, main) = corpus().into_iter().next().unwrap();
    let b = encode(&m, main).unwrap();
    assert_eq!(
        decode(&with_flags(&b, 1)).map(|d| d.flags),
        Ok(FLAG_VERIFIED)
    );
    for bit in 1..64 {
        let e = decode(&with_flags(&b, 1 | (1u64 << bit))).unwrap_err();
        assert!(e.contains("reserved PIRB flag bits"), "bit {bit}: {e}");
        let e = decode(&with_flags(&b, 1u64 << bit)).unwrap_err();
        assert!(e.contains("not marked verified"), "bit {bit} alone: {e}");
    }
    assert!(decode(&with_flags(&b, 0))
        .unwrap_err()
        .contains("not marked verified"));
    assert!(decode(&with_flags(&b, u64::MAX))
        .unwrap_err()
        .contains("reserved PIRB flag bits"));
}

#[test]
fn unverified_modules_are_refused() {
    let m = Module {
        funcs: vec![Func {
            name: "f".into(),
            kind: FuncKind::Defined,
            params: vec![],
            ret: Type::Bool,
            slots: vec![],
            slot_names: vec![],
            values: vec![],
            blocks: vec![Block {
                params: vec![],
                insts: vec![],
                term: Term::Ret(0),
            }],
        }],
        ..Default::default()
    };
    assert!(encode(&m, None).is_err());
}

#[test]
fn corrupted_input_never_panics() {
    let mut checked = 0usize;
    for (name, m, main) in corpus() {
        let b = encode(&m, main).unwrap();
        // Every strict prefix is malformed.
        for k in 0..b.len() {
            assert!(decode(&b[..k]).is_err(), "{name}: prefix {k} decoded");
            checked += 1;
        }
        // Single-byte corruptions are caught by the checksum; with the
        // checksum recomputed they reach the parser, which must return
        // (Ok or Err), never panic.
        let mut x: u32 = 0x9e37_79b9;
        for _ in 0..b.len().min(400) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let mut c = b.clone();
            let i = (x as usize) % c.len();
            c[i] = c[i].wrapping_add(1 + (x >> 24) as u8 % 255);
            assert!(
                decode(&c).is_err(),
                "{name}: corruption at {i} not detected"
            );
            reseal(&mut c);
            let _ = decode(&c);
            checked += 1;
        }
    }
    assert!(checked > 10_000);
}

/// spec/pir-bridge.md §2.3: an unverified copy (flags 0), as `pegasus-mlir
/// export` writes it, is refused by `decode` and certified by `certify` into
/// exactly the verified original.
#[test]
fn certify_turns_unverified_files_into_the_verified_original() {
    use pegasus_pir::bridge::{certify, CertifyError};
    let mut n = 0;
    for (name, m, main) in corpus() {
        let b = encode(&m, main).unwrap();
        let unverified = with_flags(&b, 0);
        assert!(
            decode(&unverified)
                .unwrap_err()
                .contains("not marked verified"),
            "{name}"
        );
        assert_eq!(certify(&unverified).as_deref(), Ok(&b[..]), "{name}");
        assert_eq!(
            certify(&b).as_deref(),
            Ok(&b[..]),
            "{name}: certifying a verified file is the identity"
        );
        n += 1;
    }
    assert!(n >= 15);
    let (_, m, main) = corpus().into_iter().next().unwrap();
    let b = encode(&m, main).unwrap();
    assert!(
        matches!(certify(&with_flags(&b, 2)), Err(CertifyError::Malformed(e)) if e.contains("reserved"))
    );
    assert!(matches!(
        certify(&b[..b.len() - 1]),
        Err(CertifyError::Malformed(_))
    ));
}

/// A module the verifier rejects is not certified, whatever its flags say.
#[test]
fn certify_runs_the_verifier() {
    use pegasus_pir::bridge::{certify, CertifyError};
    // fn f() -> i32 { %0 = const true; ret %0 }: returns a bool as i32. Built
    // by hand and serialized through a valid sibling: encode refuses it, so
    // patch the constant of a valid module instead.
    let ok = Module {
        funcs: vec![Func {
            name: "f".into(),
            kind: FuncKind::Defined,
            params: vec![],
            ret: Type::Bool,
            slots: vec![],
            slot_names: vec![],
            values: vec![Type::Bool],
            blocks: vec![Block {
                params: vec![],
                insts: vec![Inst {
                    result: Some(0),
                    kind: InstKind::Const(Const::Bool(true)),
                }],
                term: Term::Ret(0),
            }],
        }],
        ..Default::default()
    };
    let mut b = encode(&ok, None).unwrap();
    // The function's return type is the Bool type tag (2) right after the
    // (empty) parameter list; make it i32 (tag 0, int kind 2) by rewriting
    // ret type: find "params=[] ret=Bool" = [0x00, 0x02] after the name.
    let name_at = b
        .windows(2)
        .position(|w| w == [1, b'f'])
        .expect("function name");
    let ret_at = name_at + 2 /* len, 'f' */ + 1 /* kind */ + 1 /* params len */;
    assert_eq!(b[ret_at], 2, "ret type tag is Bool");
    let n = b.len() - 4;
    let mut patched = b[..ret_at].to_vec();
    patched.extend_from_slice(&[0, 2]); // Int(i32)
    patched.extend_from_slice(&b[ret_at + 1..n]);
    patched.extend_from_slice(&[0, 0, 0, 0]);
    reseal(&mut patched);
    b = patched;
    match certify(&with_flags(&b, 0)) {
        Err(CertifyError::Invalid(errs)) => assert!(!errs.is_empty()),
        other => panic!("expected the verifier to refuse it, got {other:?}"),
    }
}

/// `Const::Char` holds a Rust `char`, so an invalid scalar value cannot reach
/// the verifier: the decoder is where the invariant is enforced, at exactly
/// the boundaries the MLIR dialect checks (spec/mlir.md §2).
#[test]
fn char_constants_are_unicode_scalars_at_the_decoder() {
    let module_with = |c: char| Module {
        funcs: vec![Func {
            name: "c".into(),
            kind: FuncKind::Defined,
            params: vec![],
            ret: Type::Char,
            slots: vec![],
            slot_names: vec![],
            values: vec![Type::Char],
            blocks: vec![Block {
                params: vec![],
                insts: vec![Inst {
                    result: Some(0),
                    kind: InstKind::Const(Const::Char(c)),
                }],
                term: Term::Ret(0),
            }],
        }],
        ..Default::default()
    };
    for c in ['\0', '\u{D7FF}', '\u{E000}', '\u{10FFFF}'] {
        let b = encode(&module_with(c), None).unwrap();
        assert!(decode(&b).is_ok(), "{c:?} must round-trip");
    }
    // Same-length LEB128 rewrites of a valid scalar into an invalid one.
    for (valid, invalid, what) in [
        ('\u{D7FF}', [0x80u8, 0xB0, 0x03], "U+D800 (surrogate)"),
        (
            '\u{10FFFF}',
            [0x80u8, 0x80, 0x44],
            "0x110000 (beyond U+10FFFF)",
        ),
    ] {
        let b = encode(&module_with(valid), None).unwrap();
        let mut leb = Vec::new();
        let mut x = valid as u32;
        loop {
            let byte = (x & 0x7f) as u8;
            x >>= 7;
            if x == 0 {
                leb.push(byte);
                break;
            }
            leb.push(byte | 0x80);
        }
        let at = b
            .windows(leb.len())
            .position(|w| w == leb.as_slice())
            .expect("char constant");
        let mut bad = b.clone();
        bad[at..at + 3].copy_from_slice(&invalid);
        reseal(&mut bad);
        assert!(
            decode(&bad).unwrap_err().contains("bad char constant"),
            "{what}"
        );
        assert!(
            pegasus_pir::bridge::certify(&bad).is_err(),
            "{what}: certify must refuse it too"
        );
    }
}
