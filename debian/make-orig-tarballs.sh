#!/bin/sh
# Copyright (c) 2026 Robert August Vincent II <pillarsdotnet@gmail.com>
# Co-author: Claude Code.
#
# Build the two upstream tarballs for the Debian source package from git,
# and regenerate debian/copyright's vendored-crate section to match:
#
#   ../timesheet_<ver>.orig.tar.xz         the repository, without debian/
#   ../timesheet_<ver>.orig-vendor.tar.xz  every crate in Cargo.lock
#
# Launchpad's builders have no network, so the crates travel with the source.
# Crates that only build on Windows, macOS or WebAssembly are reduced to their
# Cargo.toml and an empty lib.rs: cargo still needs their manifests to resolve
# Cargo.lock, but nothing on Linux compiles them. They are half the vendor
# directory, and the WebAssembly ones carry prebuilt .wasm and .o files.
#
# The upstream commit is the newest one that changes anything outside debian/,
# so committing packaging changes does not change the upstream version. Pass a
# commit to override. Run with network access; prints the upstream version to
# use in debian/changelog.
set -eu

top=$(git rev-parse --show-toplevel)
cd "$top"
ref=${1:-$(git log -1 --format=%H -- . ':(exclude)debian')}
base=$(git show "$ref:Cargo.toml" | sed -n 's/^version = "\(.*\)"/\1/p' | head -n 1)
date=$(git log -1 --format=%cd --date=format:%Y%m%d "$ref")
sha=$(git rev-parse --short=7 "$ref")
ver="$base+git$date.$sha"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git archive --format=tar --prefix="timesheet-$ver/" "$ref" -- . ':(exclude)debian' \
  | xz -9 -T0 > "../timesheet_$ver.orig.tar.xz"

git archive "$ref" | tar -x -C "$work"
(cd "$work" && cargo vendor --locked --quiet vendor > /dev/null)

python3 - "$work/vendor" "$top/debian/copyright" <<'PY'
import collections, json, os, re, shutil, sys, tomllib

vendor, copyright_path = sys.argv[1], sys.argv[2]

# Stub the crates that only build for Windows, macOS or WebAssembly.
foreign = re.compile(r"^(windows.*|objc2.*|block2|dispatch2"
                     r"|wasi.*|wasm.*|wit-.*|js-sys|web-sys)$")
for name in sorted(os.listdir(vendor)):
    if not foreign.match(name):
        continue
    crate = os.path.join(vendor, name)
    with open(os.path.join(crate, ".cargo-checksum.json")) as f:
        package_sum = json.load(f)["package"]
    for entry in os.listdir(crate):
        if entry == "Cargo.toml":
            continue
        path = os.path.join(crate, entry)
        if os.path.isdir(path):
            shutil.rmtree(path)
        else:
            os.remove(path)
    os.makedirs(os.path.join(crate, "src"))
    with open(os.path.join(crate, "src", "lib.rs"), "w") as f:
        f.write("// Stubbed for the Debian package: this crate only builds "
                "for non-Linux targets.\n")
    with open(os.path.join(crate, ".cargo-checksum.json"), "w") as f:
        json.dump({"files": {}, "package": package_sum}, f)

# Group the crates by license, for debian/copyright.
def dep5(expr):
    expr = expr.replace("/", " OR ")
    expr = expr.replace("Apache-2.0 WITH LLVM-exception", "Apache-2.0-with-LLVM-exception")
    expr = expr.replace("-or-later", "+")
    # "(A OR B) AND C" is written "A or B, and C" in DEP-5.
    expr = re.sub(r"\)\s+AND\s+", ", and ", expr)
    expr = re.sub(r"\s+AND\s+\(", ", and ", expr)
    expr = expr.replace("(", "").replace(")", "")
    expr = re.sub(r"\s+OR\s+", " or ", expr)
    expr = re.sub(r"\s+AND\s+", " and ", expr)
    # Sort a plain "or" list, so equivalent licenses share one paragraph.
    if " and " not in expr:
        expr = " or ".join(sorted(expr.split(" or ")))
    return expr

groups = collections.defaultdict(list)
for name in sorted(os.listdir(vendor)):
    with open(os.path.join(vendor, name, "Cargo.toml"), "rb") as f:
        package = tomllib.load(f)["package"]
    groups[dep5(package.get("license", "UNKNOWN"))].append(name)

# Replace the vendor/ paragraphs, keeping every other paragraph in place;
# the generated ones go before the first stand-alone License paragraph.
with open(copyright_path) as f:
    paragraphs = f.read().strip("\n").split("\n\n")
kept = [p for p in paragraphs if not p.startswith("Files: vendor/")]
at = next(i for i, p in enumerate(kept) if p.startswith("License:"))
generated = [
    "Files: " + "\n ".join(f"vendor/{c}/*" for c in crates)
    + "\nCopyright: the authors of each crate, as listed in its Cargo.toml"
    + f"\nLicense: {license}"
    for license, crates in sorted(groups.items())
]
with open(copyright_path, "w") as f:
    f.write("\n\n".join(kept[:at] + generated + kept[at:]) + "\n")
PY

tar --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime="$(git log -1 --format=%cI "$ref")" -C "$work" -cf - vendor \
  | xz -9 -T0 > "../timesheet_$ver.orig-vendor.tar.xz"

echo "$ver"
