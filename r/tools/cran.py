#!/usr/bin/env python3
"""Makes the R package's source package for CRAN, which builds manby into the
package offline.

manby and the crates of its workspace, as cargo packages them, and the
crates it needs from crates.io go in src/rust/vendor.tar.xz, with the Cargo.lock that names them, and their
authors and licenses in inst/AUTHORS; then R CMD build makes the package in
the current directory. A crate that none of CRAN's platforms builds is left
as its manifest alone, and the crates' tests, benchmarks and examples are
left out, which cargo doesn't check once their checksums list no files.

    python3 r/tools/cran.py

Needs cargo and R. The workspace's crates, such as tippecanoe-rs, are those
of the checkout, so the package can be made before they are published, as
on the release PR.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile

R = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO = os.path.dirname(R)
# what CRAN builds on: Linux, macOS, and Windows with Rtools
TARGETS = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-gnu",
]
LEFT_OUT = {"tests", "test", "testdata", "test-data", "test_data", "benches", "examples", "fuzz", ".github"}
CONFIG = """[source.crates-io]
replace-with = "vendored"

[source.vendored]
directory = "vendor"
"""


def run(*args, cwd=None, capture=False):
    print("+", " ".join(args), file=sys.stderr)
    done = subprocess.run(args, cwd=cwd, check=True, text=True, stdout=subprocess.PIPE if capture else None)
    return done.stdout


def used(rust):
    """The packages that some target of CRAN's builds, as name and version."""
    packages = set()
    for target in TARGETS:
        out = run(
            "cargo", "tree", "--locked", "--offline", "--target", target, "-e", "normal,build",
            "--prefix", "none", "--format", "{p}", cwd=rust, capture=True,
        )
        for line in out.splitlines():
            m = re.match(r"(\S+) v(\S+)", line)
            if m:
                packages.add((m.group(1), m.group(2)))
    return packages


def stub(crate):
    """Leaves a crate as its manifest, its licenses and an empty library."""
    for name in os.listdir(crate):
        path = os.path.join(crate, name)
        if name in ("Cargo.toml", ".cargo-checksum.json") or name.upper().startswith(("LICENSE", "COPYING")):
            continue
        shutil.rmtree(path) if os.path.isdir(path) else os.remove(path)
    os.makedirs(os.path.join(crate, "src"))
    open(os.path.join(crate, "src", "lib.rs"), "w").close()
    build = re.search(r'^build = "([^"]+)"', open(os.path.join(crate, "Cargo.toml")).read(), re.M)
    if build:
        path = os.path.join(crate, build.group(1))
        os.makedirs(os.path.dirname(path), exist_ok=True)
        open(path, "w").write("fn main() {}\n")


def trim(crate):
    """Leaves out a crate's tests, benchmarks and examples."""
    for name in os.listdir(crate):
        if name in LEFT_OUT and os.path.isdir(os.path.join(crate, name)):
            shutil.rmtree(os.path.join(crate, name))


def no_files(crate):
    """Keeps the package's checksum, but no file's, so that cargo checks none."""
    path = os.path.join(crate, ".cargo-checksum.json")
    checksum = json.load(open(path))
    json.dump({"files": {}, "package": checksum.get("package")}, open(path, "w"))


def local_paths(path, here, local):
    """Points a packaged manifest's dependencies on the workspace's crates at
    their copies, by path from `here`."""
    lines = open(path).read().splitlines()
    out, i = [], 0
    while i < len(lines):
        line = lines[i]
        out.append(line)
        m = re.match(r"^\[(?:target\..+\.)?(?:build-)?dependencies\.([^\]]+)\]$", line)
        if m:
            section = []
            j = i + 1
            while j < len(lines) and not lines[j].startswith("["):
                section.append(lines[j])
                j += 1
            package = next((re.match(r'package = "([^"]+)"', l).group(1) for l in section if l.startswith("package = ")), m.group(1))
            if package in local:
                out.append('path = "%s"' % os.path.relpath(local[package], here).replace(os.sep, "/"))
        i += 1
    open(path, "w").write("\n".join(out) + "\n")


def authors(rust, packages):
    """The crates' authors and licenses, for inst/AUTHORS."""
    meta = json.loads(run("cargo", "metadata", "--locked", "--offline", "--format-version", "1", cwd=rust, capture=True))
    lines = [
        "manby builds the Rust crates below into the package. Their authors, as",
        "each crate names them, hold their copyrights, under the licenses given.",
        "",
    ]
    for p in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
        if (p["name"], p["version"]) not in packages or p["name"] in ("manby-r",):
            continue
        who = ", ".join(re.sub(r"\s*<[^>]*>", "", a) for a in p.get("authors") or []) or "its authors"
        lines.append("%s %s: %s; %s" % (p["name"], p["version"], who, p.get("license") or p.get("license_file") or "see the crate"))
    return "\n".join(lines) + "\n"


def main():
    version = re.search(r"^Version:\s*(\S+)", open(os.path.join(R, "DESCRIPTION")).read(), re.M).group(1)
    work = tempfile.mkdtemp(prefix="manby-cran-")
    try:
        rust = os.path.join(work, "rust")
        os.makedirs(os.path.join(rust, "src"))
        # manby and the crates of its workspace, as cargo packages them, manby
        # in rust/manby and the others in rust/manby/crates
        package = os.path.join(work, "target", "package")
        run("cargo", "package", "--workspace", "--no-verify", "--allow-dirty", "--target-dir", os.path.join(work, "target"), cwd=REPO)
        local = {}
        for crate in sorted(f for f in os.listdir(package) if f.endswith(".crate")):
            name = re.match(r"(.+)-\d+\.\d+\.\d+.*\.crate$", crate).group(1)
            with tarfile.open(os.path.join(package, crate)) as t:
                t.extractall(package, filter="data")
            local[name] = os.path.join(rust, "manby") if name == "manby" else os.path.join(rust, "manby", "crates", name)
        shutil.move(os.path.join(package, [f for f in os.listdir(package) if f.startswith("manby-") and not f.endswith(".crate") and f[len("manby-"):][0].isdigit()][0]), local["manby"])
        os.makedirs(os.path.join(rust, "manby", "crates"), exist_ok=True)
        for name, dest in local.items():
            if name != "manby":
                shutil.move(os.path.join(package, [f for f in os.listdir(package) if re.match(re.escape(name) + r"-\d", f) and not f.endswith(".crate")][0]), dest)
        if not re.search(r'^version = "%s"' % re.escape(version), open(os.path.join(rust, "manby", "Cargo.toml")).read(), re.M):
            sys.exit("manby's version is not the package's, %s" % version)
        for name, dest in local.items():
            trim(dest)
            path = os.path.join(dest, "Cargo.toml")
            # their profiles, which cargo ignores but for warning about them
            kept, profile = [], False
            for line in open(path).read().splitlines():
                if line.startswith("["):
                    profile = line.startswith("[profile")
                if not profile:
                    kept.append(line)
            open(path, "w").write("\n".join(kept) + "\n")
            local_paths(path, dest, local)
        shutil.copy(os.path.join(R, "src", "rust", "src", "lib.rs"), os.path.join(rust, "src"))
        manifest = open(os.path.join(R, "src", "rust", "Cargo.toml.in")).read().replace("@MANBY@", 'path = "manby"')
        open(os.path.join(rust, "Cargo.toml"), "w").write(manifest)

        run("cargo", "generate-lockfile", cwd=rust)
        run("cargo", "vendor", "--locked", "--versioned-dirs", "vendor", cwd=rust, capture=True)
        os.makedirs(os.path.join(rust, ".cargo"))
        open(os.path.join(rust, ".cargo", "config.toml"), "w").write(CONFIG)

        packages = used(rust)
        vendor = os.path.join(rust, "vendor")
        stubbed = 0
        for name in sorted(os.listdir(vendor)):
            crate = os.path.join(vendor, name)
            m = re.match(r"(.+)-(\d+\.\d+\.\d+.*)$", name)
            if m and (m.group(1), m.group(2)) not in packages:
                stub(crate)
                stubbed += 1
            else:
                trim(crate)
            no_files(crate)
        print("%d crates, %d of them stubs" % (len(os.listdir(vendor)), stubbed), file=sys.stderr)

        # the build that CRAN makes, offline, before the crates are packed
        run("cargo", "check", "--offline", "--locked", "--lib", cwd=rust)

        xz = os.path.join(R, "src", "rust", "vendor.tar.xz")
        with tarfile.open(xz, "w:xz", preset=9) as t:
            for name in ("vendor", "manby", ".cargo"):
                t.add(os.path.join(rust, name), arcname=name)
        shutil.copy(os.path.join(rust, "Cargo.lock"), os.path.join(R, "src", "rust", "Cargo.lock"))
        os.makedirs(os.path.join(R, "inst"), exist_ok=True)
        open(os.path.join(R, "inst", "AUTHORS"), "w").write(authors(rust, packages))
        print("vendor.tar.xz: %.1f MB" % (os.path.getsize(xz) / 1e6), file=sys.stderr)
    finally:
        shutil.rmtree(work)
    run("R", "CMD", "build", R)


if __name__ == "__main__":
    main()
