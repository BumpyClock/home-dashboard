#!/usr/bin/env python3

import hashlib
import importlib.util
import itertools
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest


sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("prepare_femtovg", HERE / "prepare-femtovg-patch.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def directory_hash(path):
    digest = hashlib.sha256()
    for entry in sorted(path.rglob("*")):
        if entry.is_file():
            digest.update(entry.relative_to(path).as_posix().encode())
            digest.update(entry.read_bytes())
    return digest.hexdigest()


class PreparationTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="pf-patch-test-")
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.source = self.root / "source"
        self.source.mkdir()
        for name in helper.BUILD_INPUTS:
            target = self.source / name
            original = HERE.parent / name
            if original.is_file():
                shutil.copy2(original, target)
            else:
                target.mkdir()
        shutil.copytree(HERE.parent / "patches", self.source / "patches")
        self.metadata = self.root / "metadata.json"
        self.metadata.write_bytes(METADATA.read_bytes())
        self.output = self.root / "output"

    def package(self, name):
        return next(p for p in json.loads(self.metadata.read_text())["packages"] if p["name"] == name)

    def test_both_corrections_and_independent_toggles_leave_inputs_unchanged(self):
        watched = [HERE.parent / "Cargo.toml", HERE.parent / "Cargo.lock"]
        for name, file in [(helper.CRATE, "itemrenderer.rs"), (helper.QUARTER_CRATE, "src/lib.rs")]:
            watched.append(Path(self.package(name)["manifest_path"]).parent / file)
        before = {p: p.read_bytes() for p in watched}
        registries = [Path(self.package(name)["manifest_path"]).parent for name in (helper.CRATE, helper.QUARTER_CRATE)]
        before_registry = {p: directory_hash(p) for p in registries}
        manifest = (self.source / "Cargo.toml").read_bytes()
        for image_aa, quarter_turn in itertools.product([False, True], repeat=2):
            with self.subTest(image_aa=image_aa, quarter_turn=quarter_turn):
                app = helper.prepare(self.source, self.metadata, self.output, image_aa, quarter_turn)
                self.assertTrue(app.is_relative_to(self.output))
                self.assertEqual((app / "Cargo.lock").read_bytes(), (self.source / "Cargo.lock").read_bytes())
                text = (app / "Cargo.toml").read_text()
                entries = text.split("[patch.crates-io]\n", 1)[1] if image_aa or quarter_turn else ""
                names = {line.split("=", 1)[0].strip() for line in entries.splitlines()}
                self.assertEqual(helper.CRATE in names, image_aa)
                self.assertEqual(helper.QUARTER_CRATE in names, quarter_turn)
                if image_aa:
                    result = (app.parent / helper.CRATE / "itemrenderer.rs").read_bytes()
                    original = before[watched[2]]
                    self.assertEqual(result, original.replace(helper.ORIGINAL_LINE, helper.PATCHED_LINE))
                if quarter_turn:
                    result = (app.parent / helper.QUARTER_CRATE / "src/lib.rs").read_bytes()
                    self.assertEqual(hashlib.sha256(result).hexdigest(), helper.QUARTER_RESULT_SHA256)
                if not image_aa and not quarter_turn:
                    self.assertEqual((app / "Cargo.toml").read_bytes(), manifest)
                self.assertFalse((app / ".git").exists())
                self.assertFalse((app / "target").exists())
        self.assertEqual(before, {p: p.read_bytes() for p in watched})
        self.assertEqual(before_registry, {p: directory_hash(p) for p in registries})

    def test_rejects_changed_registry_source(self):
        for name, file, image_aa, quarter_turn in [
            (helper.CRATE, "itemrenderer.rs", True, False),
            (helper.QUARTER_CRATE, "src/lib.rs", False, True),
        ]:
            with self.subTest(crate=name):
                data = json.loads(METADATA.read_text())
                package = next(p for p in data["packages"] if p["name"] == name)
                copied = self.root / "registry" / "src" / "test" / name
                shutil.copytree(Path(package["manifest_path"]).parent, copied)
                package["manifest_path"] = str(copied / "Cargo.toml")
                target = copied / file
                target.write_bytes(target.read_bytes() + b"\n// changed\n")
                self.metadata.write_text(json.dumps(data))
                with self.assertRaisesRegex(ValueError, "checksum"):
                    helper.prepare(self.source, self.metadata, self.output, image_aa, quarter_turn)
                self.assertFalse(self.output.exists())

    def test_rejects_changed_or_escaping_patch(self):
        for name, image_aa, quarter_turn in [
            ("slint-1.18.1-image-aa.patch", True, False),
            ("femtovg-0.27.0-quarter-turn-atlas.patch", False, True),
        ]:
            with self.subTest(patch=name):
                target = self.source / "patches" / name
                target.write_bytes(target.read_bytes().replace(b"+++ b/", b"+++ b/../../", 1))
                with self.assertRaisesRegex(ValueError, "pinned patch"):
                    helper.prepare(self.source, self.metadata, self.output, image_aa, quarter_turn)
                self.assertFalse(self.output.exists())

    def test_rejects_unpinned_metadata_and_existing_patch_section(self):
        for field, value in [("source", None), ("version", "0.28.0")]:
            data = json.loads(METADATA.read_text())
            package = next(p for p in data["packages"] if p["name"] == helper.QUARTER_CRATE)
            package[field] = value
            self.metadata.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "one registry package"):
                helper.prepare(self.source, self.metadata, self.output, False, True)
        self.metadata.write_bytes(METADATA.read_bytes())
        manifest = self.source / "Cargo.toml"
        manifest.write_bytes(manifest.read_bytes() + b"\n[patch.crates-io]\n")
        with self.assertRaisesRegex(ValueError, "already contains"):
            helper.prepare(self.source, self.metadata, self.output)
        self.assertFalse(self.output.exists())

    def test_rejects_output_inside_checkout_or_registry(self):
        with self.assertRaisesRegex(ValueError, "outside the application"):
            helper.prepare(self.source, self.metadata, self.source / "generated")
        registry = Path(self.package(helper.QUARTER_CRATE)["manifest_path"]).parent.parents[2]
        for enabled in [False, True]:
            with self.assertRaisesRegex(ValueError, "outside the Cargo registry"):
                helper.prepare(self.source, self.metadata, registry / "generated", False, enabled)

    def test_patch_line_endings_do_not_change_semantics(self):
        for target in (self.source / "patches").glob("*.patch"):
            target.write_bytes(target.read_bytes().replace(b"\r\n", b"\n").replace(b"\n", b"\r\n"))
        app = helper.prepare(self.source, self.metadata, self.output)
        self.assertEqual(hashlib.sha256((app.parent / helper.QUARTER_CRATE / "src/lib.rs").read_bytes()).hexdigest(), helper.QUARTER_RESULT_SHA256)


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} /external/locked-metadata.json")
    METADATA = Path(sys.argv[1]).resolve(strict=True)
    unittest.main(argv=[sys.argv[0]])
