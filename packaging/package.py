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
import subprocess
import tarfile
import tomllib
import zipfile


def package(platform: str, binary_dir: Path, output: Path) -> Path:
    root = Path(__file__).resolve().parent.parent
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    suffix = ".exe" if platform == "windows" else ""
    binaries = [binary_dir / (name + suffix) for name in ("ember", "ember-sign-in")]
    for binary in binaries:
        if not binary.is_file():
            raise FileNotFoundError(f"Build both Ember and its sign-in helper first: {binary}")
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
        # Sign nested executables before the bundle. Ad-hoc signatures are for
        # local/CI validation only, not Developer ID distribution.
        identity = os.environ.get("EMBER_SIGN_IDENTITY", "-")
        for path in [*(destination / binary.name for binary in binaries), staging]:
            command = ["codesign", "--force", "--sign", identity]
            if identity != "-":
                command += ["--options", "runtime", "--timestamp", "--entitlements", str(root / "packaging/macos.entitlements")]
            subprocess.run([*command, str(path)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(staging)], check=True)
        archive = output / f"Ember-{version}-macos.zip"
        subprocess.run(["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(staging), str(archive)], check=True)
    elif platform == "windows":
        shutil.copy2(root / "packaging/install-windows.ps1", staging / "install.ps1")
        shutil.copy2(root / "packaging/ember.svg", staging)
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
