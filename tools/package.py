"""Build the Windows release package.

Usage: python tools/package.py [--tag vX.Y.Z] [--out EMPTY_DIRECTORY]

Writes taffymeters-vX.Y.Z-windows-amd64.zip (exe, LICENSE, THIRD_PARTY_LICENSES.txt,
README.md, CHANGELOG.md) and release-notes.md. The exe links the C runtime statically so
it runs without the Visual C++ Redistributable.

--tag is for release builds: it must equal v + the Cargo.toml version, and the
CHANGELOG section of that version must carry a date.
Requires cargo-about: cargo install cargo-about --locked --features cli
"""
import argparse
import ctypes as c
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parent.parent
TARGET = "x86_64-pc-windows-msvc"
# Font licenses shipped next to the fonts epaint embeds; its SPDX expression omits Hack's
# and emoji-icon-font's.
FONT_LICENSES = {"epaint_default_fonts": ["fonts/Hack-Regular.txt", "fonts/emoji-icon-font-mit-license.txt"]}
# Fonts whose license files carry no copyright line; OFL 1.1 and the Ubuntu Font Licence
# require the notice, which these fonts keep only in their name table.
FONT_COPYRIGHTS = {"epaint_default_fonts": ["fonts/NotoEmoji-Regular.ttf", "fonts/Ubuntu-Light.ttf"]}


def font_copyright(path):
    """The copyright string (name ID 0) of a TrueType font."""
    data = path.read_bytes()
    (tables,) = struct.unpack_from(">H", data, 4)
    for index in range(tables):
        tag, _, offset, _ = struct.unpack_from(">4sIII", data, 12 + 16 * index)
        if tag != b"name":
            continue
        _, count, strings = struct.unpack_from(">HHH", data, offset)
        for record in range(count):
            platform, _, _, name_id, length, start = struct.unpack_from(
                ">HHHHHH", data, offset + 6 + 12 * record)
            if name_id == 0:
                raw = data[offset + strings + start:offset + strings + start + length]
                # Platforms 0 (Unicode) and 3 (Windows) store UTF-16BE; 1 (Macintosh) Roman.
                return raw.decode("utf-16-be" if platform in (0, 3) else "mac-roman").strip()
    fail(f"{path} has no copyright string")


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def run(args, **kwargs):
    print("$", " ".join(args), flush=True)
    result = subprocess.run(args, cwd=ROOT, **kwargs)
    if result.returncode != 0:
        fail(f"{args[0]} exited with {result.returncode}")
    return result


def release_notes(version, dated):
    text = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
    section = re.search(rf"^## \[{re.escape(version)}\] - (.+?)\s*$\n(.*?)(?=^## \[|^\[[^\]]+\]: |\Z)",
                        text, re.M | re.S)
    if section is None:
        fail(f"CHANGELOG.md has no section for {version}")
    if dated and not re.fullmatch(r"\d{4}-\d{2}-\d{2}", section.group(1)):
        fail(f"the CHANGELOG section for {version} is not dated: {section.group(1)}")
    return section.group(2).strip() + "\n"


def build():
    environment = os.environ.copy()
    # With an explicit --target, RUSTFLAGS skips build scripts and proc macros.
    environment["RUSTFLAGS"] = (environment.get("RUSTFLAGS", "") + " -C target-feature=+crt-static").strip()
    run(["cargo", "build", "--release", "--locked", "--target", TARGET, "--target-dir", "target/package"],
        env=environment)
    return ROOT / "target" / "package" / TARGET / "release" / "taffymeters.exe"


def product_version(exe):
    version = c.WinDLL("version")
    size = version.GetFileVersionInfoSizeW(str(exe), None)
    data = c.create_string_buffer(size)
    if not size or not version.GetFileVersionInfoW(str(exe), 0, size, data):
        fail(f"{exe} has no version resource")
    value, length = c.c_wchar_p(), c.c_uint()
    if not version.VerQueryValueW(data, r"\StringFileInfo\040904B0\ProductVersion",
                                  c.byref(value), c.byref(length)):
        fail(f"{exe} has no ProductVersion")
    return value.value


def third_party_licenses(out):
    path = out / "THIRD_PARTY_LICENSES.txt"
    run(["cargo", "about", "generate", "--locked", "--fail", "--config", "tools/about.toml",
         "--output-file", str(path), "tools/about.hbs"])
    metadata = json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked",
                               "--filter-platform", TARGET], capture_output=True).stdout)

    def directory(name, setting):
        package = next((p for p in metadata["packages"] if p["name"] == name), None)
        if package is None:
            fail(f"{name} is no longer a dependency; update {setting}")
        return package["version"], Path(package["manifest_path"]).parent

    with path.open("a", encoding="utf-8", newline="\n") as file:
        for name, files in FONT_COPYRIGHTS.items():
            version, root = directory(name, "FONT_COPYRIGHTS")
            file.write(f"\n{'=' * 80}\n{name} {version}: copyright notices of the embedded fonts\n\n")
            for relative in files:
                file.write(f"{relative}: {font_copyright(root / relative)}\n")
        for name, files in FONT_LICENSES.items():
            version, root = directory(name, "FONT_LICENSES")
            for relative in files:
                file.write(f"\n{'=' * 80}\n{name} {version}: {relative}\n\n")
                file.write((root / relative).read_text(encoding="utf-8").strip() + "\n")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--tag", help="release tag; must be v + the Cargo.toml version")
    parser.add_argument("--out", type=Path, default=ROOT / "target" / "dist",
                        help="nonexistent or empty output directory (default target/dist)")
    args = parser.parse_args()
    out = args.out.resolve()
    if out.exists() and (not out.is_dir() or any(out.iterdir())):
        parser.error("--out must be a nonexistent or empty directory")

    version = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]
    if args.tag is not None and args.tag != f"v{version}":
        fail(f"tag {args.tag} does not match the Cargo.toml version {version}")
    notes = release_notes(version, dated=args.tag is not None)

    exe = build()
    if (found := product_version(exe)) != version:
        fail(f"exe ProductVersion is {found}, expected {version}")
    if b"vcruntime140" in exe.read_bytes().lower():
        fail("exe still imports the Visual C++ runtime")

    out.mkdir(parents=True, exist_ok=True)
    licenses = third_party_licenses(out)
    name = f"taffymeters-v{version}-windows-amd64.zip"
    with zipfile.ZipFile(out / name, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        archive.write(exe, "taffymeters.exe")
        for document in ("LICENSE", "README.md", "CHANGELOG.md"):
            archive.write(ROOT / document, document)
        archive.write(licenses, licenses.name)
    licenses.unlink()
    (out / "release-notes.md").write_text(notes, encoding="utf-8", newline="\n")
    print(out / name)


if __name__ == "__main__":
    main()
