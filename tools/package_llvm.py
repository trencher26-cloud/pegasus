#!/usr/bin/env python3
"""Smoke-test, package and record the pinned LLVM/MLIR toolchain
(spec/native.md §7.1). Used by .github/workflows/toolchain.yml; `smoke` is
also run by CI on every unpacked package.

    package_llvm.py smoke --prefix DIR [--work DIR]
        The install prefix must carry the provisioning stamp and pass
        check_toolchain.py --require-stamp; its clang must compile and run a
        C program; mlir-opt, mlir-translate and mlir-tblgen must run; and
        tools/llvm_smoke must configure through the installed MLIR CMake
        package, build against the installed headers and libraries, and
        lower a module to LLVM IR.

    package_llvm.py pack --prefix DIR --platform P --out DIR
        Write <archive>.000, .001, ... (parts under GitHub's 2 GiB asset
        limit; their concatenation is a deterministic tar.xz of the prefix)
        and <archive>.manifest.json.

    package_llvm.py record --manifest FILE [--manifest FILE ...]
        Copy verified manifests into toolchain-packages.lock.json.
"""
import argparse, datetime, json, lzma, os, platform as pyplatform, shutil, subprocess, sys, tarfile, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from toolchain_lock import llvm_pin  # noqa: E402
from toolchain_packages import (BUILDER_WORKFLOW, FORMAT, PACKAGES_LOCK, PART_SIZE, PLATFORMS,  # noqa: E402
                                archive_name, load_packages, release_tag, sha256_file)

EXE = ".exe" if os.name == "nt" else ""


def run(cmd, **kw):
    print("+ " + " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run(cmd, check=True, **kw)


def smoke(prefix, work):
    pin = llvm_pin()
    prefix = os.path.abspath(prefix)
    clang = os.path.join(prefix, "bin", "clang" + EXE)
    run([sys.executable, os.path.join(HERE, "check_toolchain.py"), clang, "--require-stamp"])
    for f in ("lib/cmake/llvm/LLVMConfig.cmake", "lib/cmake/mlir/MLIRConfig.cmake",
              "include/mlir/IR/MLIRContext.h", "include/llvm/IR/Module.h"):
        if not os.path.exists(os.path.join(prefix, f)):
            sys.exit(f"FAIL: package incomplete: {f} missing")
    work = os.path.abspath(work or tempfile.mkdtemp(prefix="pg-llvm-smoke-"))
    os.makedirs(work, exist_ok=True)
    hello = os.path.join(work, "hello" + EXE)
    run([clang, os.path.join(HERE, "llvm_smoke", "hello.c"), "-o", hello])
    out = subprocess.run([hello], check=True, capture_output=True, text=True).stdout
    if f"clang {pin['version']}" not in out:
        sys.exit(f"FAIL: package clang built a program that reports {out!r}")
    print(out.strip())
    for tool in ("mlir-opt", "mlir-translate", "mlir-tblgen"):
        v = subprocess.run([os.path.join(prefix, "bin", tool + EXE), "--version"], check=True,
                           capture_output=True, text=True).stdout
        if pin["version"] not in v:
            sys.exit(f"FAIL: {tool} --version does not report {pin['version']}: {v!r}")
    bld = os.path.join(work, "cmake")
    run(["cmake", "-S", os.path.join(HERE, "llvm_smoke"), "-B", bld, "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release",
         f"-DMLIR_DIR={os.path.join(prefix, 'lib', 'cmake', 'mlir')}",
         f"-DLLVM_DIR={os.path.join(prefix, 'lib', 'cmake', 'llvm')}",
         f"-DPEGASUS_EXPECT_LLVM_VERSION={pin['version']}"])
    run(["cmake", "--build", bld])
    r = subprocess.run([os.path.join(bld, "mlir_smoke" + EXE)], capture_output=True, text=True)
    print(r.stdout[-400:], r.stderr[-400:])
    if r.returncode != 0 or "lower and translate OK" not in r.stdout:
        sys.exit(f"FAIL: MLIR smoke program exited {r.returncode}")
    print("package smoke test passed: stamp, clang, MLIR tools, MLIR CMake package, lowering to LLVM IR")


def tar_filter(ti):
    ti.uid = ti.gid = 0
    ti.uname = ti.gname = ""
    ti.mtime = 0
    return ti


def host_info():
    info = {"os": pyplatform.platform(), "machine": pyplatform.machine(),
            "runner_image": os.environ.get("ImageOS", ""), "runner_image_version": os.environ.get("ImageVersion", "")}
    if os.name != "nt":
        info["glibc"] = " ".join(pyplatform.libc_ver())
        info["cxx"] = subprocess.run(["c++", "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    else:
        r = subprocess.run(["cl"], capture_output=True, text=True)
        info["cxx"] = (r.stderr or r.stdout).splitlines()[0] if (r.stderr or r.stdout) else ""
    info["cmake"] = subprocess.run(["cmake", "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    return info


def pack(prefix, plat, out):
    if plat not in PLATFORMS:
        sys.exit(f"unknown platform {plat}")
    pin = llvm_pin()
    prefix = os.path.abspath(prefix)
    stamp = json.load(open(os.path.join(prefix, "pegasus-toolchain.json"), encoding="utf-8"))
    if stamp.get("commit") != pin["commit"]:
        sys.exit("FAIL: the prefix's stamp does not match toolchain.lock")
    os.makedirs(out, exist_ok=True)
    name = archive_name(pin, plat)
    whole = os.path.join(out, name)
    # Deterministic member order and metadata; xz preset 3 balances time
    # (single-threaded stdlib) and size.
    with lzma.open(whole, "wb", preset=3) as xz, tarfile.open(fileobj=xz, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for dirpath, dirnames, filenames in os.walk(prefix):
            dirnames.sort()
            # os.walk lists a symlink to a directory among dirnames and does
            # not descend into it: archive it as the link it is.
            links = [d for d in dirnames if os.path.islink(os.path.join(dirpath, d))]
            dirnames[:] = [d for d in dirnames if d not in links]
            for f in sorted(filenames + links):
                full = os.path.join(dirpath, f)
                tar.add(full, arcname=os.path.relpath(full, prefix).replace(os.sep, "/"),
                        recursive=False, filter=tar_filter)
    digest, size = sha256_file(whole), os.path.getsize(whole)
    parts = []
    with open(whole, "rb") as f:
        k = 0
        while True:
            chunk = f.read(PART_SIZE)
            if not chunk:
                break
            pname = f"{name}.{k:03d}"
            with open(os.path.join(out, pname), "wb") as p:
                p.write(chunk)
            parts.append({"name": pname, "sha256": sha256_file(os.path.join(out, pname)), "size": len(chunk)})
            k += 1
    os.remove(whole)
    repo = os.environ.get("GITHUB_REPOSITORY", "")
    manifest = {
        "format": FORMAT, "platform": plat,
        "source": pin["source"], "release": pin["release"], "commit": pin["commit"],
        "version": pin["version"], "projects": pin["projects"],
        "tag": release_tag(pin), "archive": name, "sha256": digest, "size": size, "parts": parts,
        "clang_sha256": stamp.get("clang_sha256"),
        "repository": repo,
        "signer_workflow": f"{repo}/{BUILDER_WORKFLOW}" if repo else "",
        "builder": {"run_id": os.environ.get("GITHUB_RUN_ID", ""), "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
                    "commit": os.environ.get("GITHUB_SHA", ""), "host": host_info(),
                    "created": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")},
    }
    mpath = os.path.join(out, name + ".manifest.json")
    with open(mpath, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
        f.write("\n")
    print(f"packed {name}: {size} bytes, sha256 {digest}, {len(parts)} part(s)")


def record(manifests):
    pin = llvm_pin()
    data = load_packages()
    for m in manifests:
        e = json.load(open(m, encoding="utf-8"))
        for k in ("commit", "version", "release"):
            if e.get(k) != pin[k]:
                sys.exit(f"FAIL: {m}: {k} {e.get(k)!r} does not match toolchain.lock")
        keep = {k: e[k] for k in ("platform", "source", "release", "commit", "version", "projects", "tag", "archive",
                                  "sha256", "size", "parts", "clang_sha256", "repository", "signer_workflow")}
        keep["builder"] = {k: e["builder"][k] for k in ("run_id", "run_attempt", "commit", "host", "created")}
        data.setdefault("packages", {})[e["platform"]] = keep
    data["format"] = FORMAT
    with open(PACKAGES_LOCK, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, sort_keys=True)
        f.write("\n")
    print(f"recorded {', '.join(sorted(data['packages']))} in {os.path.basename(PACKAGES_LOCK)}")


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("smoke")
    s.add_argument("--prefix", required=True)
    s.add_argument("--work")
    p = sub.add_parser("pack")
    p.add_argument("--prefix", required=True)
    p.add_argument("--platform", required=True)
    p.add_argument("--out", required=True)
    r = sub.add_parser("record")
    r.add_argument("--manifest", action="append", required=True)
    a = ap.parse_args()
    if a.cmd == "smoke":
        smoke(a.prefix, a.work)
    elif a.cmd == "pack":
        pack(a.prefix, a.platform, a.out)
    else:
        record(a.manifest)


main()
