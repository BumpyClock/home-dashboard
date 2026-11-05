#!/usr/bin/env python3
"""Resolve pinned Arch Linux ARM packages against the repo databases.

Usage: resolve-alarm-packages.py MIRROR < pins
Each stdin line is "name version" (epoch stripped). Each output line is
"repo<TAB>filename<TAB>sha256" from core.db / extra.db, so the caller can
verify every download. The mirror must be HTTPS.
"""
import io
import sys
import tarfile
import urllib.request
from concurrent.futures import ThreadPoolExecutor

REPOS = ("core", "extra")


def entries(mirror, repo):
    with urllib.request.urlopen(f"{mirror}/{repo}/{repo}.db", timeout=120) as response:
        data = response.read()
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:*") as db:
        for member in db:
            if not member.name.endswith("/desc"):
                continue
            description = db.extractfile(member)
            if description is None:
                raise ValueError(f"package description is not a file: {member.name}")
            fields, key = {}, None
            for line in description.read().decode().splitlines():
                if line.startswith("%") and line.endswith("%"):
                    key = line.strip("%")
                elif line and key and key not in fields:
                    fields[key] = line
            yield fields


def main():
    mirror = sys.argv[1].rstrip("/")
    if not mirror.startswith("https://"):
        sys.exit(f"mirror must use https: {mirror}")
    index = {}
    with ThreadPoolExecutor(max_workers=2) as workers:
        databases = [workers.submit(lambda repo=repo: list(entries(mirror, repo))) for repo in REPOS]
        for repo, database in zip(REPOS, databases, strict=True):
            for fields in database.result():
                index.setdefault(fields["NAME"], (repo, fields))
    for line in sys.stdin:
        if not line.strip():
            continue
        name, pinned = line.split(maxsplit=1)
        pinned = pinned.strip()
        if name not in index:
            sys.exit(f"MISSING PACKAGE: {name}")
        repo, fields = index[name]
        version = fields["VERSION"].split(":", 1)[-1]
        if version != pinned:
            print(f"NOTE: pinned {name} {pinned}, mirror has {version}; using it", file=sys.stderr)
        print(f"{repo}\t{fields['FILENAME']}\t{fields['SHA256SUM']}")


if __name__ == "__main__":
    main()
