import hashlib
import unittest
import tempfile
from pathlib import Path
import check


class BoundaryTests(unittest.TestCase):
    def test_rust_aliases_grouped_imports_reexports_and_fully_qualified_io(self):
        for source in [
            "use skiff_core as core; core::git::diff();",
            "use skiff_core::{config as c, git}; c::load();",
            "pub use skiff_client::ledger as rows;",
            "std::fs::read(path);", "use std::{fs as disk}; disk::read(path);",
            "tokio::process::Command::new(p);", "use std::env as e; e::var(x);",
            "use crate::config as c; c::list_projects();",
            "include!(\"local_owner.rs\");",
            "use std::path::Path; Path::new(p).exists();",
        ]:
            with self.subTest(source=source): self.assertTrue(check.risks(source, ".rs"))

    def test_typescript_aliases_reexports_dynamic_and_direct_io(self):
        for source in [
            'import { invoke as send } from "@tauri-apps/api/core"; send("list_projects");',
            'export { readFile } from "@tauri-apps/plugin-fs";',
            'const disk = await import("node:fs");',
            'const disk = await import(`@tauri-apps/plugin-fs`);',
            'import * as shell from "@tauri-apps/plugin-shell";',
            'globalThis.fetch(url);', 'new WebSocket(url);', 'require("fs").readFileSync(p);',
        ]:
            with self.subTest(source=source): self.assertTrue(check.risks(source, ".ts"))

    def test_comments_strings_lifetimes_and_contract_imports(self):
        self.assertEqual(check.risks('// std::fs\n/* outer /* config */ */ fn f(x: &\'static str) {}', '.rs'), [])
        self.assertEqual(check.risks('const X: &str = r#"std::fs"#; use skiff_host_api::Hello;', '.rs'), [])
        self.assertEqual(check.risks('use skiff_client::host::HostConnection;', '.rs'), [])

    def test_exact_baseline_cannot_admit_new_access_using_an_existing_alias(self):
        source = 'use skiff_core::config as c; fn old() { c::load(); }'
        entry = lambda s: {'sha256': hashlib.sha256(s.encode()).hexdigest(), 'reasons': check.risks(s, '.rs')}
        baseline = {'old.rs': entry(source)}
        self.assertEqual(check.compare(baseline, baseline), [])
        self.assertTrue(check.compare({'old.rs': entry(source + ' fn new() { c::add_project(); }')}, baseline))
        self.assertTrue(check.compare({'new.rs': entry(source)}, baseline))
        self.assertTrue(check.compare({}, baseline))

    def test_contract_dependency_and_io_rules_are_enforced(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            crate = root / 'crates/skiff-host-api'
            (crate / 'src').mkdir(parents=True)
            manifest = '[dependencies]\nserde.workspace = true\nbase64.workspace = true\n'
            (crate / 'Cargo.toml').write_text(manifest)
            (crate / 'src/lib.rs').write_text('use std::fmt;')
            self.assertEqual(check.api_errors(root), [])
            (crate / 'Cargo.toml').write_text(manifest + 'tokio.workspace = true\n')
            self.assertTrue(check.api_errors(root))
            (crate / 'Cargo.toml').write_text(manifest)
            (crate / 'src/lib.rs').write_text('std::fs::read(path);')
            self.assertTrue(check.api_errors(root))


if __name__ == '__main__': unittest.main()
