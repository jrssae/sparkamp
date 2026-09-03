//! The macOS build's licence boundary, enforced against the real dependency
//! graph rather than against what `Cargo.toml` appears to say.
//!
//! Sparkamp is AGPL-3.0-only and the App Store build ships that licence
//! knowingly. What it cannot ship is somebody else's copyleft: an App Store
//! binary is redistributed under terms that a GPL or AGPL dependency forbids,
//! and unlike the Linux build there is no system package manager holding those
//! libraries at arm's length. Every third-party crate the Mac links has to be
//! permissive.
//!
//! GStreamer gets its own check because a licence scan will not catch it. The
//! Rust bindings are MIT, so nothing here would object to them, but linking
//! them pulls in the LGPL C library and whichever plugins the pipeline
//! resolves at runtime, some of which are GPL. `Cargo.toml` keeps them behind
//! `cfg(not(target_os = "macos"))` for that reason. This asserts the gate
//! holds, because the gate is one line and deleting it compiles fine on Linux.
//!
//! Both tests read `cargo metadata --filter-platform`, which resolves the graph
//! for a target the way a build for it would. That is the difference between
//! this and grepping the manifest: a transitive dependency that arrives three
//! crates down, or a gate someone widened, shows up here.

use std::collections::{HashMap, HashSet};
use std::process::Command;

/// Apple targets a released build covers. A universal binary is both.
const APPLE_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

/// One crate in a resolved graph, reduced to what a licence audit needs.
struct Crate {
    name: String,
    version: String,
    license: String,
    is_ours: bool,
}

/// Resolve the dependency graph as a build for `target` would see it.
///
/// `--filter-platform` prunes `resolve.nodes` to that target; `packages` stays
/// unpruned, so the node list is what decides membership and the package list
/// only supplies the metadata.
fn graph_for(target: &str) -> Vec<Crate> {
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--offline",
            "--filter-platform",
            target,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap_or_else(|e| panic!("could not run cargo metadata for {target}: {e}"));

    assert!(
        out.status.success(),
        "cargo metadata failed for {target}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("cargo metadata did not return JSON");

    let ours: HashSet<&str> = meta["workspace_members"]
        .as_array()
        .expect("no workspace_members")
        .iter()
        .map(|id| id.as_str().expect("workspace member id is not a string"))
        .collect();

    let by_id: HashMap<&str, &serde_json::Value> = meta["packages"]
        .as_array()
        .expect("no packages")
        .iter()
        .map(|p| (p["id"].as_str().expect("package id is not a string"), p))
        .collect();

    let nodes = meta["resolve"]["nodes"]
        .as_array()
        .expect("no resolve graph; --filter-platform needs a resolved lock file");

    nodes
        .iter()
        .map(|n| {
            let id = n["id"].as_str().expect("resolve node id is not a string");
            let p = by_id
                .get(id)
                .unwrap_or_else(|| panic!("{id} is in the graph but not in packages"));
            Crate {
                name: p["name"].as_str().unwrap_or_default().to_string(),
                version: p["version"].as_str().unwrap_or_default().to_string(),
                // A crate with no `license` field carries a `license_file`
                // instead, which cargo does not read. Unknown is not permissive,
                // so it is reported rather than waved through.
                license: p["license"]
                    .as_str()
                    .unwrap_or("UNDECLARED")
                    .to_string(),
                is_ours: ours.contains(id),
            }
        })
        .collect()
}

/// No third-party crate the Mac links may be GPL or AGPL. Sparkamp's own
/// crates are AGPL and are the whole of the exception.
#[test]
fn the_macos_graph_carries_no_copyleft_but_our_own() {
    for target in APPLE_TARGETS {
        let graph = graph_for(target);
        assert!(
            graph.len() > 50,
            "{target} resolved only {} crates, which is too few to be the real \
             graph; the query is broken, not the graph",
            graph.len()
        );

        let offenders: Vec<String> = graph
            .iter()
            .filter(|c| !c.is_ours)
            .filter(|c| c.license.contains("GPL") || c.license == "UNDECLARED")
            .map(|c| format!("{} {} ({})", c.name, c.version, c.license))
            .collect();

        assert!(
            offenders.is_empty(),
            "{target} links crates the App Store build cannot carry:\n  {}",
            offenders.join("\n  ")
        );
    }
}

/// No GStreamer binding may reach an Apple target, whatever its own licence.
#[test]
fn the_macos_graph_links_no_gstreamer() {
    for target in APPLE_TARGETS {
        let found: Vec<String> = graph_for(target)
            .iter()
            .filter(|c| c.name.starts_with("gstreamer"))
            .map(|c| format!("{} {}", c.name, c.version))
            .collect();

        assert!(
            found.is_empty(),
            "{target} resolves GStreamer, so the cfg gate in Cargo.toml has \
             stopped holding:\n  {}",
            found.join("\n  ")
        );
    }
}
