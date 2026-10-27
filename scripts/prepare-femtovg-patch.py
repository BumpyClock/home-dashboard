#!/usr/bin/env python3
"""Prepare isolated app and renderer crates for the selected pinned corrections."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


CRATE = "i-slint-renderer-femtovg"
VERSION = "1.18.1"
SOURCE_SHA256 = "30ee92fa6c8ce12c21fb7135f3696677131bb252d9ba34b7a5e0079687e3d82b"
ORIGINAL_LINE = b"            .with_anti_alias(fit.source_to_target_x != 1. || fit.source_to_target_y != 0.);"
PATCHED_LINE = b"            .with_anti_alias(fit.source_to_target_x != 1. || fit.source_to_target_y != 1.);"
AA_PATCH_SHA256 = "f1285490a4167d88bb8161d9ec0bf7a0a94c8ed139c6ff81ea0647d0f87a316b"
QUARTER_CRATE = "femtovg"
QUARTER_VERSION = "0.27.0"
QUARTER_SOURCE_SHA256 = "a2448395b61ba674fcb485336299e87a11dc5013d5dbf947385a2930ddef1e15"
QUARTER_PATCH_SHA256 = "14455d1a7bd9ef33cede08e4700ed32b32867ce9ca47ef1cb8cd9d482d2edb1c"
QUARTER_RESULT_SHA256 = "b292e8053ef77e96b96de0a01932e09bc6d7c8ce3ef1f1ea31b8b14576363915"
BUILD_INPUTS = ("Cargo.toml", "Cargo.lock", "build.rs", ".cargo", "src", "ui", "assets", "instrumentation", "renderer")


def registry_crate(packages: list, name: str, version: str, output: Path) -> Path:
    matches = [p for p in packages if p["name"] == name and p["version"] == version]
    package_source = matches[0].get("source") if len(matches) == 1 else None
    if not isinstance(package_source, str) or not package_source.startswith("registry+"):
        raise ValueError(f"metadata must contain one registry package {name} {version}")
    crate = Path(matches[0]["manifest_path"]).resolve(strict=True).parent
    if output.is_relative_to(crate) or output.is_relative_to(crate.parents[2]):
        raise ValueError("output must be outside the Cargo registry")
    return crate


def pinned_patch(path: Path, checksum: str) -> bytes:
    patch = path.read_bytes().replace(b"\r\n", b"\n")
    if hashlib.sha256(patch).hexdigest() != checksum:
        raise ValueError(f"{path.name} checksum does not match the pinned patch")
    return patch


def prepare(source: Path, metadata_path: Path, output: Path, image_aa: bool = True, quarter_turn: bool = True, render_trace: bool = False, texture_prewarm: bool = False) -> Path:
    source = source.resolve(strict=True)
    metadata_path = metadata_path.resolve(strict=True)
    output = output.resolve()
    if not source.is_dir() or output.is_relative_to(source):
        raise ValueError("output must be outside the application source directory")

    packages = json.loads(metadata_path.read_text(encoding="utf-8"))["packages"]
    for package in packages:
        if str(package.get("source", "")).startswith("registry+"):
            crate = Path(package["manifest_path"]).resolve().parent
            if len(crate.parents) >= 3 and output.is_relative_to(crate.parents[2]):
                raise ValueError("output must be outside the Cargo registry")
    selected = []
    if image_aa:
        crate = registry_crate(packages, CRATE, VERSION, output)
        original = (crate / "itemrenderer.rs").read_bytes()
        if hashlib.sha256(original).hexdigest() != SOURCE_SHA256 or original.count(ORIGINAL_LINE) != 1:
            raise ValueError("Slint itemrenderer.rs checksum does not match the pinned source")
        pinned_patch(source / "patches" / "slint-1.18.1-image-aa.patch", AA_PATCH_SHA256)
        selected.append((CRATE, crate, original))
    if quarter_turn:
        crate = registry_crate(packages, QUARTER_CRATE, QUARTER_VERSION, output)
        original = (crate / "src/lib.rs").read_bytes()
        if hashlib.sha256(original).hexdigest() != QUARTER_SOURCE_SHA256:
            raise ValueError("FemtoVG src/lib.rs checksum does not match the pinned source")
        patch = pinned_patch(source / "patches" / "femtovg-0.27.0-quarter-turn-atlas.patch", QUARTER_PATCH_SHA256)
        if not shutil.which("patch"):
            raise ValueError("the patch tool is required for the quarter-turn correction")
        selected.append((QUARTER_CRATE, crate, patch))

    trace_inputs = []
    if render_trace:
        pins = json.loads((source / "patches/render-trace-pins.json").read_text())
        for name, pin in pins.items():
            crate = registry_crate(packages, name, pin["version"], output)
            for filename, hashes in pin["files"].items():
                if hashlib.sha256((crate / filename).read_bytes()).hexdigest() != hashes["source"]:
                    raise ValueError(f"{name}/{filename} checksum does not match the pinned trace source")
            data = pinned_patch(source / "patches" / pin["patch"], pin["sha256"])
            trace_inputs.append((name, crate, pin, data))

    texture_input = None
    if texture_prewarm:
        pin = json.loads((source / "patches/texture-prewarm-pins.json").read_text())
        crate = registry_crate(packages, CRATE, pin["version"], output)
        for filename, hashes in pin["files"].items():
            if hashlib.sha256((crate / filename).read_bytes()).hexdigest() != hashes["source"]:
                raise ValueError(f"{filename} checksum does not match the pinned texture source")
        data = pinned_patch(source / "patches" / pin["patch"], pin["sha256"])
        texture_input = (crate, pin, data)

    manifest = (source / "Cargo.toml").read_bytes()
    if re.search(rb"(?m)^\s*\[\s*patch\.crates-io\s*\]", manifest):
        raise ValueError("application already contains a [patch.crates-io] section")
    for name in BUILD_INPUTS:
        if not (source / name).exists():
            raise ValueError(f"application build input missing: {name}")

    output.mkdir(parents=True, exist_ok=True)
    prepared = Path(tempfile.mkdtemp(prefix="femtovg-pinned-", dir=output))
    ignore = shutil.ignore_patterns(".git", "target", "__pycache__", ".cargo-ok", ".cargo-checksum.json")
    patch_entries = []
    for name, crate, data in selected:
        patched_crate = prepared / name
        shutil.copytree(crate, patched_crate, ignore=ignore)
        if name == CRATE:
            (patched_crate / "itemrenderer.rs").write_bytes(data.replace(ORIGINAL_LINE, PATCHED_LINE))
        else:
            result = subprocess.run(
                ["patch", "--batch", "--forward", "--fuzz=0", "--no-backup-if-mismatch", "--reject-file=-", "-p1"],
                input=data, cwd=patched_crate, capture_output=True,
            )
            if result.returncode != 0:
                raise ValueError(f"quarter-turn patch failed: {result.stderr.decode(errors='replace').strip()}")
            if hashlib.sha256((patched_crate / "src/lib.rs").read_bytes()).hexdigest() != QUARTER_RESULT_SHA256:
                raise ValueError("patched FemtoVG source checksum does not match")
        patch_entries.append(f"{name} = {{ path = {json.dumps(patched_crate.as_posix())} }}\n")

    app = prepared / "app-source"
    app.mkdir()
    for name in BUILD_INPUTS:
        entry = source / name
        if entry.is_dir():
            shutil.copytree(entry, app / name, ignore=ignore)
        else:
            shutil.copy2(entry, app / name)
    for name, crate, pin, data in trace_inputs:
        patched_crate = prepared / name
        if not patched_crate.exists():
            shutil.copytree(crate, patched_crate, ignore=ignore)
            patch_entries.append(f"{name} = {{ path = {json.dumps(patched_crate.as_posix())} }}\n")
        result = subprocess.run(
            ["patch", "--batch", "--forward", "--fuzz=0", "--no-backup-if-mismatch", "--reject-file=-", "-p1"],
            input=data, cwd=patched_crate, capture_output=True,
        )
        if result.returncode != 0:
            raise ValueError(f"{name} trace patch failed: {result.stderr.decode(errors='replace').strip()}")
        for filename, hashes in pin["files"].items():
            if hashlib.sha256((patched_crate / filename).read_bytes()).hexdigest() != hashes["result"]:
                raise ValueError(f"{name}/{filename} patched trace checksum does not match")
        with (patched_crate / "Cargo.toml").open("a") as manifest_file:
            manifest_file.write("\n[dependencies.frame-trace]\npath = " + json.dumps(str(app / "instrumentation/frame-trace")) + '\nfeatures = ["render-hooks"]\n')

    if texture_input:
        crate, pin, data = texture_input
        patched_crate = prepared / CRATE
        if not patched_crate.exists():
            shutil.copytree(crate, patched_crate, ignore=ignore)
            patch_entries.append(f"{CRATE} = {{ path = {json.dumps(patched_crate.as_posix())} }}\n")
        result = subprocess.run(
            ["patch", "--batch", "--forward", "--fuzz=0", "--no-backup-if-mismatch", "--reject-file=-", "-p1"],
            input=data, cwd=patched_crate, capture_output=True,
        )
        if result.returncode != 0:
            raise ValueError(f"texture patch failed: {result.stderr.decode(errors='replace').strip()}")
        for filename, hashes in pin["files"].items():
            if hashlib.sha256((patched_crate / filename).read_bytes()).hexdigest() not in hashes["results"]:
                raise ValueError(f"{filename} patched texture checksum does not match")
        with (patched_crate / "Cargo.toml").open("a") as manifest_file:
            manifest_file.write("\n[dependencies.texture-prewarm]\npath = " + json.dumps(str(app / "renderer/texture-prewarm")) + '\nfeatures = ["renderer"]\n')

    patched_manifest = manifest
    if patch_entries:
        patched_manifest += ("\n[patch.crates-io]\n" + "".join(patch_entries)).encode()
    (app / "Cargo.toml").write_bytes(patched_manifest)
    return app


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("metadata", type=Path, help="cargo metadata --locked --offline JSON")
    parser.add_argument("output", type=Path, help="external PF_BUILD_DIR")
    parser.add_argument("--image-aa", choices=("0", "1"), default="1")
    parser.add_argument("--quarter-turn", choices=("0", "1"), default="1")
    parser.add_argument("--render-trace", choices=("0", "1"), default="0")
    parser.add_argument("--texture-prewarm", choices=("0", "1"), default="0")
    args = parser.parse_args()
    try:
        print(prepare(args.source, args.metadata, args.output, args.image_aa == "1", args.quarter_turn == "1", args.render_trace == "1", args.texture_prewarm == "1"))
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"prepare-femtovg-patch: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
