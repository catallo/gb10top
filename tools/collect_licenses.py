"""Copy upstream notices for the crates and standard library used by a release build."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tomllib


def main():
    destination = Path(sys.argv[1])
    manifests = {
        Path(message["manifest_path"])
        for message in map(json.loads, sys.stdin)
        if message["reason"] == "compiler-artifact"
    }
    # The release includes gb10top's LICENSE at the root of the archive.
    manifests.remove(Path("Cargo.toml").resolve())
    for manifest in sorted(manifests):
        package = tomllib.loads(manifest.read_text())["package"]
        crate = manifest.parent
        output = destination / f"{package['name']}-{package['version']}"
        notices = [
            path for path in crate.rglob("*")
            if path.is_file() and path.name.upper().startswith(
                ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE", "UNLICENSE")
            )
        ]
        if not notices:
            raise ValueError(f"Missing license text for {output.name}")
        for path in notices:
            target = output / path.relative_to(crate)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)

    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    rust_docs = Path(sysroot) / "share/doc/rust"
    std = destination / "rust-std"
    shutil.copytree(rust_docs / "licenses", std / "licenses")
    shutil.copyfile(rust_docs / "COPYRIGHT-library.html", std / "COPYRIGHT-library.html")


if __name__ == "__main__":
    main()
