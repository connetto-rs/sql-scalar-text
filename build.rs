//! Bundles PostgreSQL's `timezone_abbreviations` sets, `Default`, `Australia` and `India`, as
//! static tables read with the library's own parser.

extern crate alloc;

#[expect(
    dead_code,
    reason = "the build script reads the files and leaves the lookup to the library"
)]
#[path = "src/zone/tznames.rs"]
mod tznames;

use core::fmt::Write as _;
use std::{env, fs, path::Path};

use tznames::Meaning;

/// The sets PostgreSQL installs, by file name.
const SETS: &[&str] = &["Default", "Australia", "India"];

fn main() {
    let directory = Path::new("src/zone/timezonesets");
    println!("cargo::rerun-if-changed=src/zone/timezonesets");
    println!("cargo::rerun-if-changed=src/zone/tznames.rs");
    let texts: Vec<(&str, String)> = SETS
        .iter()
        .map(|&name| {
            let text = fs::read_to_string(directory.join(name))
                .unwrap_or_else(|error| panic!("read {name}: {error}"));
            (name, text)
        })
        .collect();
    let mut out = String::new();
    for &name in SETS {
        let mut files = |file: &str| {
            texts
                .iter()
                .find(|(known, _)| *known == file)
                .map(|(_, text)| text.as_str())
        };
        let mut entries = Vec::new();
        tznames::parse_file(name, 0, &mut files, &mut entries)
            .unwrap_or_else(|error| panic!("parse {name}: {error}"));
        writeln!(
            out,
            "pub(super) static {}: [Abbreviation; {}] = [",
            name.to_ascii_uppercase(),
            entries.len()
        )
        .expect("write to a String");
        for entry in &entries {
            let meaning = match &entry.meaning {
                Meaning::Fixed { offset, dst } => {
                    format!("Meaning::Fixed {{ offset: {offset}, dst: {dst} }}")
                }
                Meaning::Zone(zone) => format!("Meaning::Zone(Cow::Borrowed({zone:?}))"),
            };
            writeln!(
                out,
                "    Abbreviation {{ token: Cow::Borrowed({:?}), meaning: {meaning} }},",
                entry.token
            )
            .expect("write to a String");
        }
        out.push_str("];\n");
    }
    let target = Path::new(&env::var("OUT_DIR").expect("cargo sets OUT_DIR"))
        .join("timezone_abbreviations.rs");
    fs::write(target, out).expect("write the abbreviation tables");
}
