#!/usr/bin/env python3
"""Conservative M0 import/I/O tripwire, with exact-content legacy exemptions.

Unlike a directory allowlist, an entry admits only the reviewed bytes of one
file. Every edit to existing direct ownership must shrink or explicitly review
that entry. This intentionally over-reports client-policy adapters during M0.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name("baseline.json")
# These are module/operation identifiers, not substrings of comments or strings.
RUST_IO = {"fs", "process", "net", "env", "dirs", "libc", "include", "include_str", "include_bytes", "exists", "is_file", "is_dir", "metadata", "canonicalize", "read_dir", "read_to_string", "app_config_dir", "home_dir"}
RUST_HOST = {"skiff_core", "config", "ledger", "skipper", "agent_folder", "skiff_agent"}
JS_IO = {"invoke", "fetch", "XMLHttpRequest", "WebSocket", "require", "Deno", "Bun"}


def tokens(source, rust=False):
    """Keep string import specifiers; skip comments, including nested Rust ones."""
    out, i = [], 0
    while i < len(source):
        if source[i].isspace():
            i += 1
        elif source.startswith("//", i):
            end = source.find("\n", i)
            i = len(source) if end < 0 else end + 1
        elif source.startswith("/*", i):
            depth, i = 1, i + 2
            while depth and i < len(source):
                if source.startswith("/*", i): depth, i = depth + 1, i + 2
                elif source.startswith("*/", i): depth, i = depth - 1, i + 2
                else: i += 1
        else:
            raw = re.match(r'r(#+)?"', source[i:]) if rust else None
            if raw:
                start = i + len(raw[0])
                end = source.find('"' + (raw[1] or ''), start)
                i = len(source) if end < 0 else end + 1 + len(raw[1] or '')
                out.append(("string", source[start:end]))
            elif source[i] in '\"\'`' and not (rust and source[i] == "'" and not re.match(r"'(?:\\.|[^'\\])'", source[i:])):
                quote, start = source[i], i + 1
                i += 1
                while i < len(source) and source[i] != quote:
                    i += 2 if source[i] == "\\" else 1
                out.append(("string", source[start:i]))
                i += 1
            else:
                word = re.match(r"[A-Za-z_][A-Za-z_0-9]*", source[i:])
                if word:
                    out.append(("word", word[0])); i += len(word[0])
                else:
                    out.append(("punct", source[i])); i += 1
    return out


def risks(source, suffix):
    found = set()
    ts = tokens(source, rust=suffix == ".rs")
    for i, (kind, value) in enumerate(ts):
        if suffix == ".rs" and kind == "word":
            if value in RUST_HOST or value in RUST_IO:
                found.add(value)
            if value == "skiff_client":
                # The new connection is the sanctioned seam; grouped/aliased
                # legacy imports remain frozen rather than guessing resolution.
                following = [v for _, v in ts[i + 1:i + 4]]
                if following != [":", ":", "host"]:
                    found.add(value)
        elif suffix != ".rs":
            if kind == "word" and value in JS_IO:
                found.add(value)
            if kind == "string" and (value.startswith(("@tauri-apps/", "node:", "fs", "child_process"))
                                     or value.endswith(("/platform/os", "/platform/fileActions"))):
                found.add(value)
    return sorted(found)


def inventory(root):
    result = {}
    for folder in ["apps/desktop/src", "apps/desktop/src-tauri/src"]:
        for file in sorted((root / folder).rglob("*")):
            if file.suffix not in {".rs", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"}: continue
            text = file.read_text()
            reasons = risks(text, file.suffix)
            if reasons:
                result[str(file.relative_to(root))] = {
                    "sha256": hashlib.sha256(file.read_bytes()).hexdigest(), "reasons": reasons,
                }
    # A new dependency or alias must not introduce an uninspected I/O provider.
    for name in ["apps/desktop/src-tauri/Cargo.toml", "apps/desktop/package.json"]:
        result[name] = {"sha256": hashlib.sha256((root / name).read_bytes()).hexdigest(), "reasons": ["dependency surface"]}
    return result


def compare(actual, baseline):
    errors = []
    for file, entry in actual.items():
        old = baseline.get(file)
        if old is None or old["sha256"] != entry["sha256"]:
            errors.append(f"{file}: new/changed direct boundary access ({', '.join(entry['reasons'])}); use HostConnection or explicitly review the exact legacy baseline")
    for file in baseline.keys() - actual.keys():
        errors.append(f"{file}: obsolete exemption; remove it from the baseline")
    return errors


def api_errors(root):
    manifest = tomllib.loads((root / "crates/skiff-host-api/Cargo.toml").read_text())
    errors = []
    if manifest.get("dependencies", {}) != {"serde": {"workspace": True}, "base64": {"workspace": True}} or manifest.get("build-dependencies") or manifest.get("target"):
        errors.append("skiff-host-api: runtime dependencies must remain only serde/base64, with no build/target dependencies")
    for source in (root / "crates/skiff-host-api/src").rglob("*.rs"):
        forbidden = set(risks(source.read_text(), ".rs"))
        if forbidden: errors.append(f"{source}: I/O/application dependency in contract crate: {sorted(forbidden)}")
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--record", action="store_true", help="explicitly review and replace the legacy inventory (never run in CI)")
    args = parser.parse_args()
    actual = inventory(ROOT)
    if args.record:
        # Purpose labels make client-owned exceptions explicit and reviewable.
        old = json.loads(BASELINE.read_text()) if BASELINE.exists() else {}
        client_files = {
            "apps/desktop/src-tauri/src/clipboard.rs", "apps/desktop/src-tauri/src/header_bar.rs",
            "apps/desktop/src-tauri/src/native_menu.rs", "apps/desktop/src-tauri/src/quit.rs",
            "apps/desktop/src-tauri/src/workspace_file.rs", "apps/desktop/src/workspace/profile.ts",
            "apps/desktop/src/appearance/appTheme.ts", "apps/desktop/src/appearance/themeCards.ts",
            "apps/desktop/src/diagnostics/latency.ts", "apps/desktop/src/diagnostics/memory.ts",
            "apps/desktop/src/platform/clipboard.ts", "apps/desktop/src/terminal/terminalFont.ts",
            "apps/desktop/src/ui/infoDialogs.ts",

        }
        for file, entry in actual.items():
            client = file in client_files
            entry["ownership"] = old.get(file, {}).get("ownership", "client policy: exact existing adapter only" if client else "legacy local adapter: migrate behind Host API in M1–M6")
        BASELINE.write_text(json.dumps(actual, indent=2, sort_keys=True) + "\n")
    baseline = json.loads(BASELINE.read_text())
    errors = compare(actual, baseline) + api_errors(ROOT)
    if errors:
        print("\n".join(errors)); return 1
    print(f"Host boundary passed: {len(baseline)} exact-content baseline entries; Host API dependencies remain I/O-free")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
