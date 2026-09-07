//! Finding a corpus on disk and splitting it by class.
//!
//! The class of an image is the **directory it sits in**, not what it is called. The earlier tools
//! guessed — `photo-kodim*` or larger than four megapixels — which worked while the corpus was
//! twenty-five images chosen by hand and stops working the moment a thousand photographs arrive
//! with names nobody controls.
//!
//! A corpus root is laid out one directory per class, and optionally one directory per *source*
//! inside it:
//!
//! ```text
//! C:\brp-corpus\
//!   photo\raw-crop\      crops from CC0 raw files
//!   photo\kodak\         the eight the published figures quote
//!   synthetic\clipart\   CC0 SVG renders
//!   screenshot\phone\    F-Droid listing screenshots
//! ```
//!
//! so `photo` is the class and `raw-crop` the source. Images directly under the root take the
//! root's own name as their class, which is what keeps a flat `samples/` directory working.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One image, with the class it belongs to already decided.
#[derive(Debug, Clone)]
pub struct Subject {
    pub path: PathBuf,
    /// First directory component under the root — the class.
    pub class: String,
    /// Second component, or the class again when there is no second level.
    pub source: String,
    /// `source/file.png`, which is what a per-image table should print: the file name alone is
    /// not unique across a corpus assembled from four places.
    pub name: String,
}

/// Every image under `root`, sorted, classified, and optionally capped per class.
///
/// The cap takes images round-robin across the sources of a class rather than the first `n` in
/// sorted order, so a sample of 100 from a class assembled from two places covers both. It is
/// deterministic: the same root and the same cap always give the same subjects.
pub fn discover(root: &Path, per_class: Option<usize>) -> Result<Vec<Subject>> {
    let mut paths = Vec::new();
    walk(root, &mut paths).with_context(|| format!("reading {}", root.display()))?;
    paths.sort();
    if paths.is_empty() {
        bail!("no PNG or WebP images under {}", root.display());
    }

    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "corpus".to_string());

    // Grouped twice over: class, then source, so the cap can interleave the sources.
    let mut classes: BTreeMap<String, BTreeMap<String, Vec<Subject>>> = BTreeMap::new();
    for path in paths {
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let parts: Vec<String> = rel
            .parent()
            .map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let class = parts.first().cloned().unwrap_or_else(|| root_name.clone());
        let source = parts.get(1).cloned().unwrap_or_else(|| class.clone());
        let file = rel
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let name = format!("{source}/{file}");
        classes
            .entry(class.clone())
            .or_default()
            .entry(source.clone())
            .or_default()
            .push(Subject {
                path,
                class,
                source,
                name,
            });
    }

    let mut out = Vec::new();
    for (_, sources) in classes {
        let mut buckets: Vec<Vec<Subject>> = sources.into_values().collect();
        match per_class {
            None => buckets.into_iter().for_each(|b| out.extend(b)),
            Some(cap) => {
                let mut taken = 0;
                let mut round = 0;
                while taken < cap {
                    let mut progressed = false;
                    for bucket in &mut buckets {
                        if let Some(subject) = bucket.get(round) {
                            out.push(subject.clone());
                            taken += 1;
                            progressed = true;
                            if taken == cap {
                                break;
                            }
                        }
                    }
                    if !progressed {
                        break;
                    }
                    round += 1;
                }
            }
        }
    }
    out.sort_by(|a, b| a.class.cmp(&b.class).then_with(|| a.path.cmp(&b.path)));
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else if path.is_file() && brp_imageio::is_supported_image(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// The classes present, in the order tables should print them, each with its subjects.
///
/// Order is by name except that `photo` leads when it is there: every figure this project has
/// published is a photograph figure, and the control row belongs at the top.
pub fn by_class(subjects: &[Subject]) -> Vec<(String, Vec<usize>)> {
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, s) in subjects.iter().enumerate() {
        groups.entry(s.class.clone()).or_default().push(i);
    }
    let mut v: Vec<(String, Vec<usize>)> = groups.into_iter().collect();
    v.sort_by_key(|(name, _)| (name != "photo", name.clone()));
    v
}

/// Reads `BRP_SAMPLE`: images per class, or every image when unset or `0`.
pub fn sample_from_env() -> Option<usize> {
    match std::env::var("BRP_SAMPLE").ok()?.parse::<usize>() {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// The corpus root: the first argument, or `samples/`.
pub fn root_from_args() -> PathBuf {
    std::env::args()
        .nth(1)
        .unwrap_or_else(|| "samples".to_string())
        .into()
}
