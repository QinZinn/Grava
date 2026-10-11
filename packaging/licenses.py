#!/usr/bin/env python3
"""Collect redistributable notices for the default-feature Linux release build.

Uses only the Python standard library and Cargo's target-filtered resolved graph.
Build dependencies and proc macros are retained conservatively; dev dependencies
and inactive optional/platform dependencies are excluded. No network is used
except Cargo's normal dependency resolution/downloads. The matching rust-docs
component supplies the standard library's own copyright catalog.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from html.parser import HTMLParser
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_TARGET = "x86_64-unknown-linux-musl"
MUSL_SOURCE = "https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT?h=v1.2.5"
# Rust's official musl targets currently bundle musl 1.2.5; keep this text in
# sync with src/ci/docker/scripts/musl-toolchain.sh when upgrading the toolchain.
MUSL_COPYRIGHT_SHA256 = 'f9bc4423732350eb0b3f7ed7e91d530298476f8fec0c6c427a1c04ade22655af'
MUSL_COPYRIGHT = """musl as a whole is licensed under the following standard MIT license:

----------------------------------------------------------------------
Copyright © 2005-2020 Rich Felker, et al.

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
----------------------------------------------------------------------

Authors/contributors include:

A. Wilcox
Ada Worcester
Alex Dowad
Alex Suykov
Alexander Monakov
Andre McCurdy
Andrew Kelley
Anthony G. Basile
Aric Belsito
Arvid Picciani
Bartosz Brachaczek
Benjamin Peterson
Bobby Bingham
Boris Brezillon
Brent Cook
Chris Spiegel
Clément Vasseur
Daniel Micay
Daniel Sabogal
Daurnimator
David Carlier
David Edelsohn
Denys Vlasenko
Dmitry Ivanov
Dmitry V. Levin
Drew DeVault
Emil Renner Berthing
Fangrui Song
Felix Fietkau
Felix Janda
Gianluca Anzolin
Hauke Mehrtens
He X
Hiltjo Posthuma
Isaac Dunham
Jaydeep Patil
Jens Gustedt
Jeremy Huntwork
Jo-Philipp Wich
Joakim Sindholt
John Spencer
Julien Ramseier
Justin Cormack
Kaarle Ritvanen
Khem Raj
Kylie McClain
Leah Neukirchen
Luca Barbato
Luka Perkov
M Farkas-Dyck (Strake)
Mahesh Bodapati
Markus Wichmann
Masanori Ogino
Michael Clark
Michael Forney
Mikhail Kremnyov
Natanael Copa
Nicholas J. Kain
orc
Pascal Cuoq
Patrick Oppenlander
Petr Hosek
Petr Skocik
Pierre Carrier
Reini Urban
Rich Felker
Richard Pennington
Ryan Fairfax
Samuel Holland
Segev Finer
Shiz
sin
Solar Designer
Stefan Kristiansson
Stefan O'Rear
Szabolcs Nagy
Timo Teräs
Trutz Behn
Valentin Ochs
Will Dietz
William Haddon
William Pitcock

Portions of this software are derived from third-party works licensed
under terms compatible with the above MIT license:

The TRE regular expression implementation (src/regex/reg* and
src/regex/tre*) is Copyright © 2001-2008 Ville Laurikari and licensed
under a 2-clause BSD license (license text in the source files). The
included version has been heavily modified by Rich Felker in 2012, in
the interests of size, simplicity, and namespace cleanliness.

Much of the math library code (src/math/* and src/complex/*) is
Copyright © 1993,2004 Sun Microsystems or
Copyright © 2003-2011 David Schultz or
Copyright © 2003-2009 Steven G. Kargl or
Copyright © 2003-2009 Bruce D. Evans or
Copyright © 2008 Stephen L. Moshier or
Copyright © 2017-2018 Arm Limited
and labelled as such in comments in the individual source files. All
have been licensed under extremely permissive terms.

The ARM memcpy code (src/string/arm/memcpy.S) is Copyright © 2008
The Android Open Source Project and is licensed under a two-clause BSD
license. It was taken from Bionic libc, used on Android.

The AArch64 memcpy and memset code (src/string/aarch64/*) are
Copyright © 1999-2019, Arm Limited.

The implementation of DES for crypt (src/crypt/crypt_des.c) is
Copyright © 1994 David Burren. It is licensed under a BSD license.

The implementation of blowfish crypt (src/crypt/crypt_blowfish.c) was
originally written by Solar Designer and placed into the public
domain. The code also comes with a fallback permissive license for use
in jurisdictions that may not recognize the public domain.

The smoothsort implementation (src/stdlib/qsort.c) is Copyright © 2011
Valentin Ochs and is licensed under an MIT-style license.

The x86_64 port was written by Nicholas J. Kain and is licensed under
the standard MIT terms.

The mips and microblaze ports were originally written by Richard
Pennington for use in the ellcc project. The original code was adapted
by Rich Felker for build system and code conventions during upstream
integration. It is licensed under the standard MIT terms.

The mips64 port was contributed by Imagination Technologies and is
licensed under the standard MIT terms.

The powerpc port was also originally written by Richard Pennington,
and later supplemented and integrated by John Spencer. It is licensed
under the standard MIT terms.

All other files which have no copyright comments are original works
produced specifically for use as part of this library, written either
by Rich Felker, the main author of the library, or by one or more
contibutors listed above. Details on authorship of individual files
can be found in the git version control history of the project. The
omission of copyright and license comments in each file is in the
interest of source tree size.

In addition, permission is hereby granted for all public header files
(include/* and arch/*/bits/*) and crt files intended to be linked into
applications (crt/*, ldso/dlstart.c, and arch/*/crt_arch.h) to omit
the copyright notice and permission notice otherwise required by the
license, and to use these files without any requirement of
attribution. These files include substantial contributions from:

Bobby Bingham
John Spencer
Nicholas J. Kain
Rich Felker
Richard Pennington
Stefan Kristiansson
Szabolcs Nagy

all of whom have explicitly granted such permission.

This file previously contained text expressing a belief that most of
the files covered by the above exception were sufficiently trivial not
to be subject to copyright, resulting in confusion over whether it
negated the permissions granted in the license. In the spirit of
permissive licensing, and of not having licensing issues being an
obstacle to adoption, that text has been removed.
"""

# realfft 3.5.0 declares MIT in its manifest/README, but upstream did not
# include a license file, even in the recorded Git revision. Pin this audited
# exception and identify the verbatim upstream author separately; do not invent
# a copyright year or silently invent notices for future missing-license crates.
REALFFT_REVISION = "d0d4eee0525fd27c96c8a046d6d107acd5ed84a6"
REALFFT_SOURCE = (
    "https://github.com/HEnquist/realfft/blob/"
    + REALFFT_REVISION
    + "/Cargo.toml"
)
MIT_TERMS = """Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""


class NoticeError(Exception):
    """A required dependency notice could not be recovered."""


class PlainText(HTMLParser):
    """Preserve every copyright and license paragraph from Rust's HTML catalog."""

    BLOCKS = {
        "h1", "h2", "h3", "h4", "p", "pre", "li", "div", "details", "summary", "br"
    }

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.parts: list[str] = []
        self.preformatted = False
        self.hidden = 0

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag in {"style", "script", "head"}:
            self.hidden += 1
        if tag in self.BLOCKS:
            self.parts.append("\n")
        if tag == "pre":
            self.preformatted = True

    def handle_endtag(self, tag: str) -> None:
        if tag in {"style", "script", "head"}:
            self.hidden -= 1
        if tag in self.BLOCKS:
            self.parts.append("\n")
        if tag == "pre":
            self.preformatted = False

    def handle_data(self, data: str) -> None:
        if not self.hidden:
            self.parts.append(data if self.preformatted else re.sub(r"\s+", " ", data))

    def text(self) -> str:
        text = "".join(self.parts)
        text = re.sub(r"\n[ \t]+", "\n", text)
        return re.sub(r"\n{3,}", "\n\n", text).strip() + "\n"


def command(*args: str) -> str:
    result = subprocess.run(args, cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE)
    return result.stdout


def resolved_packages(target: str) -> tuple[dict, list[dict]]:
    metadata = json.loads(command(
        "cargo", "metadata", "--locked", "--format-version", "1",
        "--filter-platform", target,
    ))
    resolve = metadata.get("resolve")
    if not resolve or not resolve.get("root"):
        raise NoticeError("Cargo metadata did not identify the root package")
    nodes = {node["id"]: node for node in resolve["nodes"]}
    packages = {package["id"]: package for package in metadata["packages"]}
    root = resolve["root"]
    pending = [root]
    selected: set[str] = set()
    while pending:
        current = pending.pop()
        if current in selected:
            continue
        selected.add(current)
        for dependency in nodes[current]["deps"]:
            if any(edge["kind"] != "dev" for edge in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    dependencies = [packages[package_id] for package_id in selected if package_id != root]
    dependencies.sort(key=lambda package: (package["name"], package["version"]))
    return packages[root], dependencies


def is_notice(path: Path) -> bool:
    name = path.name.upper()
    return any(name == prefix or name.startswith(prefix + suffix)
               for prefix in ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE")
               for suffix in ("", "-", "_", "."))


def read_notice(path: Path) -> str:
    text = path.read_text(encoding="utf-8-sig")
    if not text.strip():
        raise NoticeError(f"Empty license/notice file: {path}")
    return text.rstrip() + "\n"


def crate_notices(package: dict) -> list[tuple[str, str]]:
    directory = Path(package["manifest_path"]).parent
    paths: set[Path] = set()
    explicit = package.get("license_file")
    if explicit:
        path = directory / explicit
        if not path.is_file():
            raise NoticeError(f"Missing explicit license_file for {package['name']}: {path}")
        paths.add(path)
    for path in directory.rglob("*"):
        # Also cover licenses/Apache-2.0.txt and similar nested notice bundles.
        license_folder = any(part.lower() in {"license", "licenses", "licence", "licences"}
                             for part in path.relative_to(directory).parts[:-1])
        if path.is_file() and (is_notice(path) or license_folder):
            paths.add(path)
    notices = [(str(path.relative_to(directory)), read_notice(path)) for path in sorted(paths)]
    if notices:
        return notices
    if (package["name"], package["version"], package.get("license")) == ("realfft", "3.5.0", "MIT"):
        vcs = json.loads((directory / ".cargo_vcs_info.json").read_text())
        if vcs.get("git", {}).get("sha1") != REALFFT_REVISION:
            raise NoticeError("realfft license exception does not match its audited Git revision")
        authors = "; ".join(package.get("authors", []))
        return [("MIT terms (explicit version-pinned upstream exception)",
                 "Upstream author attribution: " + authors + "\n"
                 "Upstream declaration: " + REALFFT_SOURCE + "\n"
                 "The published crate and its recorded upstream revision declare MIT but\n"
                 "contain no license file or copyright year. Standard MIT terms follow.\n\n"
                 + MIT_TERMS)]
    raise NoticeError(
        f"No bundled license/notice text for {package['name']} {package['version']}; "
        "add an audited notice before distributing the binary"
    )


def section(title: str, text: str) -> str:
    return "\n" + "=" * 78 + "\n" + title + "\n" + "=" * 78 + "\n\n" + text.rstrip() + "\n"


def runtime_notices(target: str) -> str:
    sysroot = Path(command("rustc", "--print", "sysroot").strip())
    docs = sysroot / "share" / "doc" / "rust"
    copyright_file = docs / "COPYRIGHT-library.html"
    if not copyright_file.is_file():
        raise NoticeError(
            "The current Rust standard-library copyright catalog is missing. "
            "Update to current stable Rust and install its rust-docs component "
            "(`rustup update stable; rustup component add --toolchain stable rust-docs`), "
            "then retry with RUSTUP_TOOLCHAIN=stable. Older toolchains, including "
            "the application's Rust 1.88 minimum, do not ship this catalog."
        )
    parser = PlainText()
    parser.feed(copyright_file.read_text(encoding="utf-8"))
    rust_version = command("rustc", "--version").strip()
    result = section(
        "Rust standard library: " + rust_version,
        "The following is the complete standard-library notice catalog shipped\n"
        "with this Rust toolchain, converted from COPYRIGHT-library.html. It also\n"
        "describes platforms and optional components not linked into this Linux\n"
        "binary; their inclusion is informational, not an assertion of use.\n\n"
        + parser.text(),
    )
    # These cover the in-tree declarations and LLVM runtime exception. Crates
    # listed by the stdlib catalog carry their own complete bundled texts there.
    for name in ("MIT", "Apache-2.0", "Unicode-3.0", "BSD-2-Clause", "LLVM-exception"):
        path = docs / "licenses" / (name + ".txt")
        if not path.is_file():
            raise NoticeError(f"Missing Rust runtime license text: {path}")
        result += section("Rust runtime license terms: " + name, read_notice(path))
    if target.endswith("-linux-musl"):
        if hashlib.sha256(MUSL_COPYRIGHT.encode()).hexdigest() != MUSL_COPYRIGHT_SHA256:
            raise NoticeError("Bundled musl COPYRIGHT does not match its pinned hash")
        result += section("musl 1.2.5: COPYRIGHT", "Source: " + MUSL_SOURCE + "\n\n" + MUSL_COPYRIGHT)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="path for THIRD-PARTY-LICENSES.txt")
    parser.add_argument("--target", default=DEFAULT_TARGET, help="Cargo target triple")
    args = parser.parse_args()
    try:
        root, packages = resolved_packages(args.target)
        text = (
            f"Third-party notices for {root['name']} {root['version']}\n"
            f"Target: {args.target}\n"
            "Cargo features: default (pulse)\n\n"
            "This file preserves bundled license choices and copyright notices.\n"
            "It includes the resolved default-feature Cargo dependency closure,\n"
            "including compile-time helpers, excluding dev-only and target-inactive\n"
            "dependencies. License alternatives remain available as stated upstream.\n"
            "The application itself is covered by the accompanying LICENSE file.\n"
        )
        for package in packages:
            expression = package.get("license")
            if not expression and not package.get("license_file"):
                raise NoticeError(f"No license declaration for {package['name']} {package['version']}")
            notices = crate_notices(package)
            info = (
                f"Declared license: {expression or 'see license_file'}\n"
                f"Source: {package.get('repository') or package.get('source') or 'local'}\n"
            )
            if package.get("authors"):
                info += "Authors: " + "; ".join(package["authors"]) + "\n"
            for name, content in notices:
                info += "\n--- " + name + " ---\n\n" + content
            text += section(f"{package['name']} {package['version']}", info)
        text += runtime_notices(args.target)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    except (NoticeError, OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"License notice generation failed: {error}", file=sys.stderr)
        return 1
    print(f"Wrote notices for {len(packages)} Cargo dependencies and static runtimes to {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
