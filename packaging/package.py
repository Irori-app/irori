#!/usr/bin/env python3
"""Package already-built Ember binaries using only Python's standard library.

    cargo build --locked --release -p ember -p ember-sign-in
    python3 packaging/package.py --platform macos --binary-dir target/release --output dist

Signing is deliberately separate: use a Developer ID for public macOS releases
and Authenticode for Windows. CI packages are unsigned test artifacts.
"""
import argparse
import os
from pathlib import Path
import plistlib
import shutil
import struct
import subprocess
import tarfile
import tomllib
import zipfile


def validate_macos_dependencies(binary: Path) -> None:
    """Fail rather than ship an app that only runs on its build machine."""
    linked = subprocess.run(["otool", "-L", str(binary)], check=True, capture_output=True, text=True)
    for line in linked.stdout.splitlines():
        if not line.startswith("\t"):
            continue
        dependency = line.strip().split(" (compatibility version", 1)[0]
        if not dependency.startswith(("/System/Library/", "/usr/lib/")):
            raise RuntimeError(
                f"{binary.name} depends on a library outside macOS: {dependency}. "
                "Enable static linking or explicitly bundle and relocate that dependency."
            )


WINDOWS_CRT_FILES = ("vcruntime140.dll", "vcruntime140_1.dll")


def validate_windows_x64(binary: Path) -> None:
    """Never mix an ARM/x86 executable with the x64 redistributable."""
    with binary.open("rb") as file:
        dos = file.read(64)
        if len(dos) != 64 or dos[:2] != b"MZ":
            raise ValueError(f"Not a Windows executable: {binary}")
        file.seek(struct.unpack_from("<I", dos, 60)[0])
        pe = file.read(6)
    if len(pe) != 6 or pe[:4] != b"PE\0\0" or struct.unpack_from("<H", pe, 4)[0] != 0x8664:
        raise ValueError(f"Windows packaging currently requires x64 binaries: {binary}")


def windows_crt_directory() -> Path:
    """Find the installed toolchain's redistributable, never arbitrary system DLLs."""
    explicit = os.environ.get("EMBER_WINDOWS_CRT_DIR")
    if explicit:
        candidates = [Path(explicit)]
    else:
        roots = []
        if directory := os.environ.get("VCToolsRedistDir"):
            roots.append(Path(directory))
        vswhere = Path(os.environ.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
        if vswhere.is_file():
            installation = subprocess.run(
                [str(vswhere), "-latest", "-products", "*", "-requires",
                 "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"],
                check=True, capture_output=True, text=True,
            ).stdout.strip()
            if installation:
                redist = Path(installation) / "VC/Redist/MSVC"
                versions = [p for p in redist.glob("*") if p.is_dir()]
                versions.sort(key=lambda p: tuple(int(x) for x in p.name.split(".") if x.isdecimal()), reverse=True)
                roots.extend(versions)
        candidates = [candidate for root in roots for candidate in root.glob("x64/Microsoft.VC*.CRT")]
    for candidate in candidates:
        if all((candidate / name).is_file() for name in WINDOWS_CRT_FILES):
            return candidate
    raise FileNotFoundError(
        "Missing x64 Visual C++ redistributable DLLs. Install the Visual Studio C++ tools, "
        "or set EMBER_WINDOWS_CRT_DIR to their x64 Microsoft.VC*.CRT directory."
    )


def package(platform: str, binary_dir: Path, output: Path) -> Path:
    root = Path(__file__).resolve().parent.parent
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    suffix = ".exe" if platform == "windows" else ""
    binaries = [binary_dir / (name + suffix) for name in ("ember", "ember-sign-in")]
    for binary in binaries:
        if not binary.is_file():
            raise FileNotFoundError(f"Build both Ember and its sign-in helper first: {binary}")
    if platform == "macos":
        for binary in binaries:
            validate_macos_dependencies(binary)
    windows_runtime = []
    if platform == "windows":
        runtime_directory = windows_crt_directory()
        windows_runtime = [runtime_directory / name for name in WINDOWS_CRT_FILES]
        for binary in [*binaries, *windows_runtime]:
            validate_windows_x64(binary)
    output.mkdir(parents=True, exist_ok=True)
    staging = output / ("Ember.app" if platform == "macos" else "Ember")
    staging.mkdir()  # Never overwrite an existing package or unrelated files.
    destination = staging / "Contents/MacOS" if platform == "macos" else staging
    destination.mkdir(parents=True, exist_ok=True)
    for binary in binaries:
        shutil.copy2(binary, destination / binary.name)
    if platform == "macos":
        resources = staging / "Contents/Resources"
        resources.mkdir()
        shutil.copy2(root / "packaging/ember.svg", resources)
        info = {
            "CFBundleName": "Ember", "CFBundleDisplayName": "Ember",
            "CFBundleIdentifier": "org.ember.Ember", "CFBundleExecutable": "ember",
            "CFBundlePackageType": "APPL", "CFBundleVersion": version,
            "CFBundleShortVersionString": version, "LSMinimumSystemVersion": "12.0",
            "LSApplicationCategoryType": "public.app-category.games",
            "NSHighResolutionCapable": True,
            "NSPrincipalClass": "NSApplication",
            "CFBundleURLTypes": [{"CFBundleURLName": "Ember game launcher",
                "CFBundleTypeRole": "Viewer", "CFBundleURLSchemes": ["ember"]}],
        }
        icon = root / "packaging/ember.icns"
        if icon.is_file():
            shutil.copy2(icon, resources)
            info["CFBundleIconFile"] = "ember.icns"
        with (staging / "Contents/Info.plist").open("wb") as file:
            plistlib.dump(info, file)
        # Sign the nested helper before the bundle. Signing CFBundleExecutable
        # directly promotes the operation to the whole app, so it must not run
        # while the helper is still unsigned (as it is on Intel builds).
        # Ad-hoc signatures are for local/CI validation, not distribution.
        identity = os.environ.get("EMBER_SIGN_IDENTITY", "-")
        for path in [destination / "ember-sign-in", staging]:
            command = ["codesign", "--force", "--sign", identity]
            if identity != "-":
                command += ["--options", "runtime", "--timestamp", "--entitlements", str(root / "packaging/macos.entitlements")]
            subprocess.run([*command, str(path)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(staging)], check=True)
        archive = output / f"Ember-{version}-macos.zip"
        subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(staging), str(archive)], check=True)
    elif platform == "windows":
        for library in windows_runtime:
            shutil.copy2(library, staging / library.name)
        shutil.copy2(root / "packaging/install-windows.ps1", staging / "install.ps1")
        shutil.copy2(root / "packaging/ember.svg", staging)
        shutil.copy2(root / "packaging/ember.ico", staging)
        archive = output / f"Ember-{version}-windows.zip"
        with zipfile.ZipFile(archive, "x", zipfile.ZIP_DEFLATED) as zip_file:
            for file in staging.rglob("*"):
                if file.is_file():
                    zip_file.write(file, file.relative_to(output))
    else:
        shutil.copy2(root / "packaging/ember.desktop", staging)
        shutil.copy2(root / "packaging/ember.svg", staging)
        archive = output / f"Ember-{version}-linux.tar.gz"
        with tarfile.open(archive, "x:gz") as tar:
            tar.add(staging, arcname="Ember")
    return archive


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=["linux", "macos", "windows"], required=True)
    parser.add_argument("--binary-dir", type=Path, default=Path("target/release"))
    parser.add_argument("--output", type=Path, default=Path("dist"))
    args = parser.parse_args()
    print(package(args.platform, args.binary_dir.resolve(), args.output.resolve()))
