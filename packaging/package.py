#!/usr/bin/env python3
"""Package already-built Irori binaries using only Python's standard library.

    cargo build --locked --release -p irori -p irori-sign-in
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
import tempfile
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


def macos_arch(binary: Path) -> str:
    """`arm64`, `x86_64` or `universal`, read from the binary rather than the build host."""
    archs = subprocess.run(["lipo", "-archs", str(binary)], check=True, capture_output=True, text=True).stdout.split()
    return "universal" if len(archs) > 1 else archs[0]


def make_dmg(app: Path, dmg: Path, identity: str) -> None:
    """A compressed disk image showing the app beside a link to /Applications."""
    with tempfile.TemporaryDirectory() as temporary:
        folder = Path(temporary) / "Irori"
        folder.mkdir()
        subprocess.run(["ditto", str(app), str(folder / app.name)], check=True)
        (folder / "Applications").symlink_to("/Applications")
        subprocess.run(["hdiutil", "create", "-quiet", "-volname", "Irori", "-srcfolder", str(folder),
                        "-fs", "HFS+", "-format", "UDZO", str(dmg)], check=True)
    if identity != "-":
        subprocess.run(["codesign", "--force", "--sign", identity, "--timestamp", str(dmg)], check=True)


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
    explicit = os.environ.get("IRORI_WINDOWS_CRT_DIR")
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
        "or set IRORI_WINDOWS_CRT_DIR to their x64 Microsoft.VC*.CRT directory."
    )


def package(platform: str, binary_dir: Path, output: Path, dmg: bool = False) -> Path:
    """Package the binaries and return the archive. On macOS, `dmg` also writes a
    disk image next to it."""
    root = Path(__file__).resolve().parent.parent
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    suffix = ".exe" if platform == "windows" else ""
    binaries = [binary_dir / (name + suffix) for name in ("irori", "irori-sign-in")]
    for binary in binaries:
        if not binary.is_file():
            raise FileNotFoundError(f"Build both Irori and its sign-in helper first: {binary}")
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
    staging = output / ("Irori.app" if platform == "macos" else "Irori")
    staging.mkdir()  # Never overwrite an existing package or unrelated files.
    destination = staging / "Contents/MacOS" if platform == "macos" else staging
    destination.mkdir(parents=True, exist_ok=True)
    for binary in binaries:
        shutil.copy2(binary, destination / binary.name)
    if platform == "macos":
        resources = staging / "Contents/Resources"
        resources.mkdir()
        shutil.copy2(root / "packaging/irori.svg", resources)
        info = {
            "CFBundleName": "Irori", "CFBundleDisplayName": "Irori",
            "CFBundleIdentifier": "app.irori.Irori", "CFBundleExecutable": "irori",
            "CFBundlePackageType": "APPL", "CFBundleVersion": version,
            "CFBundleShortVersionString": version, "LSMinimumSystemVersion": "12.0",
            "LSApplicationCategoryType": "public.app-category.games",
            "NSHighResolutionCapable": True,
            "NSPrincipalClass": "NSApplication",
            "CFBundleURLTypes": [{"CFBundleURLName": "Irori game launcher",
                "CFBundleTypeRole": "Viewer", "CFBundleURLSchemes": ["irori"]}],
        }
        icon = root / "packaging/irori.icns"
        if icon.is_file():
            shutil.copy2(icon, resources)
            info["CFBundleIconFile"] = "irori.icns"
        with (staging / "Contents/Info.plist").open("wb") as file:
            plistlib.dump(info, file)
        # Sign the nested helper before the bundle. Signing CFBundleExecutable
        # directly promotes the operation to the whole app, so it must not run
        # while the helper is still unsigned (as it is on Intel builds).
        # Ad-hoc signatures are for local/CI validation, not distribution.
        identity = os.environ.get("IRORI_SIGN_IDENTITY", "-")
        for path in [destination / "irori-sign-in", staging]:
            command = ["codesign", "--force", "--sign", identity]
            if identity != "-":
                command += ["--options", "runtime", "--timestamp", "--entitlements", str(root / "packaging/macos.entitlements")]
            subprocess.run([*command, str(path)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(staging)], check=True)
        arch = macos_arch(destination / "irori")
        archive = output / f"Irori-{version}-macos-{arch}.zip"
        subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(staging), str(archive)], check=True)
        if dmg:
            make_dmg(staging, output / f"Irori-{version}-macos-{arch}.dmg", identity)
    elif platform == "windows":
        for library in windows_runtime:
            shutil.copy2(library, staging / library.name)
        shutil.copy2(root / "packaging/install-windows.ps1", staging / "install.ps1")
        shutil.copy2(root / "packaging/irori.svg", staging)
        shutil.copy2(root / "packaging/irori.ico", staging)
        shutil.copy2(root / "LICENSE", staging / "LICENSE.txt")
        archive = output / f"Irori-{version}-windows.zip"
        with zipfile.ZipFile(archive, "x", zipfile.ZIP_DEFLATED) as zip_file:
            for file in staging.rglob("*"):
                if file.is_file():
                    zip_file.write(file, file.relative_to(output))
    else:
        shutil.copy2(root / "packaging/irori.desktop", staging)
        shutil.copy2(root / "packaging/irori.svg", staging)
        shutil.copy2(root / "packaging/linux/io.github.irori_app.Irori.metainfo.xml", staging)
        shutil.copy2(root / "LICENSE", staging)
        archive = output / f"Irori-{version}-linux.tar.gz"
        with tarfile.open(archive, "x:gz") as tar:
            tar.add(staging, arcname="Irori")
    return archive


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", choices=["linux", "macos", "windows"], required=True)
    parser.add_argument("--binary-dir", type=Path, default=Path("target/release"))
    parser.add_argument("--output", type=Path, default=Path("dist"))
    parser.add_argument("--dmg", action="store_true", help="macOS: also build a disk image")
    args = parser.parse_args()
    print(package(args.platform, args.binary_dir.resolve(), args.output.resolve(), args.dmg))
