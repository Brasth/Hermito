//! Cancellable off-thread project filesystem scan.
//! Uses `ignore` for gitignore/ignore filtering. Builds recursive model.
//! Dirs before files, alpha within. Cancels gracefully. No UI thread IO.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ignore::WalkBuilder;

use crate::document::WorkspaceEpoch;
use crate::project::tree::{EntryKind, ProjectEntry, ProjectTree};

#[derive(Clone, Debug)]
pub struct ProjectScanResult {
    pub epoch: WorkspaceEpoch,
    pub tree: ProjectTree,
}

/// Spawn cancellable scan. Returns tagged result. Caller (event loop) decides
/// refresh. Partial result on cancel is acceptable (next refresh fixes).
pub async fn scan_project(
    root: PathBuf,
    epoch: WorkspaceEpoch,
    cancel: Arc<AtomicBool>,
) -> ProjectScanResult {
    let root_for_fallback = root.clone();
    let built = tokio::task::spawn_blocking(move || {
        if cancel.load(Ordering::Relaxed) {
            return ProjectTree {
                root: root_for_fallback.clone(),
                entries: vec![],
            };
        }
        build_tree(&root_for_fallback, cancel)
    })
    .await
    .unwrap_or_else(|_| ProjectTree {
        root,
        entries: vec![],
    });

    ProjectScanResult { epoch, tree: built }
}

fn build_tree(root: &PathBuf, cancel: Arc<AtomicBool>) -> ProjectTree {
    let mut flat: Vec<(PathBuf, bool)> = vec![];

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .sort_by_file_name(|a, b| {
            // dirs first is done later; here lexical for walk stability
            a.cmp(b)
        });

    for dent in builder.build() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let dent = match dent {
            Ok(d) => d,
            Err(_) => continue,
        };
        let p = dent.path();
        if p == root.as_path() {
            continue;
        }
        let is_dir = dent.file_type().is_some_and(|t| t.is_dir());
        if let Ok(rel) = p.strip_prefix(root) {
            flat.push((rel.to_path_buf(), is_dir));
        }
    }

    let mut entries: Vec<ProjectEntry> = vec![];
    build_nested(&flat, &mut entries);

    sort_entries(&mut entries);

    ProjectTree {
        root: root.clone(),
        entries,
    }
}

/// Group by first path component, recurse for subdirs.
fn build_nested(flat: &[(PathBuf, bool)], out: &mut Vec<ProjectEntry>) {
    let mut groups: BTreeMap<String, (bool, Vec<(PathBuf, bool)>)> = BTreeMap::new();

    let mut i = 0;
    while i < flat.len() {
        let (rel, is_dir) = flat[i].clone();
        let mut comp_iter = rel.components();
        if let Some(std::path::Component::Normal(first)) = comp_iter.next() {
            let first_s = first.to_string_lossy().into_owned();
            let rest = comp_iter.as_path().to_path_buf();
            let g = groups.entry(first_s.clone()).or_insert((false, vec![]));
            if rest.as_os_str().is_empty() {
                g.0 = is_dir;
            } else {
                g.1.push((rest, is_dir));
            }
        }
        i += 1;
    }

    for (name, (is_dir_flag, subs)) in groups {
        let mut kids: Vec<ProjectEntry> = vec![];
        if is_dir_flag && !subs.is_empty() {
            build_nested(&subs, &mut kids);
        }
        out.push(ProjectEntry {
            name,
            kind: if is_dir_flag {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            children: kids,
            is_expanded: true,
        });
    }
}

fn sort_entries(v: &mut [ProjectEntry]) {
    v.sort_by(|a, b| {
        use std::cmp::Ordering;
        match (a.is_dir(), b.is_dir()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });
    for e in v.iter_mut() {
        if e.is_dir() {
            sort_entries(&mut e.children);
        }
    }
}
#[derive(Clone, Debug)]
pub struct ProjectFileLoadResult {
    pub epoch: WorkspaceEpoch,
    pub path: PathBuf,
    pub content: Option<String>,
}

/// Blocking read suitable for off-thread spawn (event loop never reads FS for project files).
pub fn load_project_file(path: PathBuf, epoch: WorkspaceEpoch) -> ProjectFileLoadResult {
    let content = std::fs::read_to_string(&path).ok();
    ProjectFileLoadResult {
        epoch,
        path,
        content,
    }
}
