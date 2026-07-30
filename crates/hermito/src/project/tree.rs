//! Recursive collapsible tree model. Sorted on construction (dirs before files).
//! resolve_path guarantees result is under workspace root (no escape).

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntryKind {
    Dir,
    File,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectEntry {
    pub name: String,
    pub kind: EntryKind,
    pub children: Vec<ProjectEntry>,
    pub is_expanded: bool,
}

impl ProjectEntry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Dir)
    }
}

#[derive(Clone, Debug)]
pub struct ProjectTree {
    pub root: PathBuf,
    pub entries: Vec<ProjectEntry>,
}

impl ProjectTree {
    /// Resolve a path composed of entry names. Returns a safe path guaranteed
    /// to be under root (prevents .. / absolute escapes). Used by open/focus.
    /// Returns None for non-matching or unsafe segment.
    pub fn resolve_path(&self, names: &[&str]) -> Option<PathBuf> {
        if names.is_empty() {
            return Some(self.root.clone());
        }
        let mut cur_children = &self.entries;
        let mut out = self.root.clone();
        for (i, &name) in names.iter().enumerate() {
            if name.is_empty() || name == ".." || name.contains('/') || name.contains('\\') {
                return None;
            }
            let entry = cur_children.iter().find(|e| e.name == name)?;
            out = out.join(&entry.name);
            if i + 1 < names.len() {
                if !entry.is_dir() {
                    return None;
                }
                cur_children = &entry.children;
            }
        }
        // final safety: must be descendant
        if out.starts_with(&self.root) {
            Some(out)
        } else {
            None
        }
    }

    /// Locate entry for a name path (for selection state etc).
    pub fn find_entry(&self, names: &[&str]) -> Option<&ProjectEntry> {
        let mut cur = &self.entries;
        let mut last = None;
        for &name in names {
            last = cur.iter().find(|e| e.name == name);
            if let Some(e) = last {
                if e.is_dir() {
                    cur = &e.children;
                }
            } else {
                return None;
            }
        }
        last
    }
    /// Number of visible rows in pre-order (dirs before files; all is_expanded=true in p1).
    pub fn visible_entry_count(&self) -> usize {
        fn count(entries: &[ProjectEntry]) -> usize {
            let mut n = 0;
            for e in entries {
                n += 1;
                if e.is_dir() && e.is_expanded {
                    n += count(&e.children);
                }
            }
            n
        }
        count(&self.entries)
    }

    /// Path segments (names) for the entry at flat 0-based visible row (pre-order).
    /// Returns None if row out of range. Used to constrain activate to tree entries only.
    pub fn entry_path_at_row(&self, row: usize) -> Option<Vec<String>> {
        let mut idx = 0usize;
        fn search(
            entries: &[ProjectEntry],
            target: usize,
            idx: &mut usize,
            path: &mut Vec<String>,
        ) -> Option<Vec<String>> {
            for e in entries {
                if *idx == target {
                    let mut p = path.clone();
                    p.push(e.name.clone());
                    return Some(p);
                }
                *idx += 1;
                if e.is_dir() && e.is_expanded {
                    path.push(e.name.clone());
                    if let Some(res) = search(&e.children, target, idx, path) {
                        return Some(res);
                    }
                    path.pop();
                }
            }
            None
        }
        search(&self.entries, row, &mut idx, &mut Vec::new())
    }

    /// Toggle expansion of dir at safe path (relative to root). Returns true if a dir was toggled.
    /// Used by Enter activation on dirs in PrimaryPane.
    pub fn toggle_path(&mut self, target: &Path) -> bool {
        if !target.starts_with(&self.root) {
            return false;
        }
        let rel = match target.strip_prefix(&self.root) {
            Ok(r) => r,
            Err(_) => return false,
        };
        if rel.as_os_str().is_empty() {
            return false;
        }
        let names: Vec<String> = rel
            .components()
            .filter_map(|c| {
                if let std::path::Component::Normal(s) = c {
                    Some(s.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect();
        if names.is_empty() {
            return false;
        }
        let segs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        self.toggle_names(&segs)
    }

    fn toggle_names(&mut self, names: &[&str]) -> bool {
        fn toggle(entries: &mut [ProjectEntry], names: &[&str]) -> bool {
            if names.is_empty() {
                return false;
            }
            let head = names[0];
            for entry in entries.iter_mut() {
                if entry.name == head {
                    if names.len() == 1 {
                        if entry.is_dir() {
                            entry.is_expanded = !entry.is_expanded;
                            return true;
                        }
                        return false;
                    } else if entry.is_dir() {
                        return toggle(&mut entry.children, &names[1..]);
                    } else {
                        return false;
                    }
                }
            }
            false
        }
        toggle(&mut self.entries, names)
    }

    /// Return visible row index for exact names path (if currently visible via expanded ancestors).
    /// Used after toggle to preserve selection identity (not numeric row which shifts on expand/collapse).
    pub fn row_for_entry_path(&self, names: &[&str]) -> Option<usize> {
        if names.is_empty() {
            return None;
        }
        let mut idx = 0usize;
        fn search(
            entries: &[ProjectEntry],
            target: &[&str],
            idx: &mut usize,
            cur_path: &mut Vec<String>,
        ) -> Option<usize> {
            for e in entries {
                cur_path.push(e.name.clone());
                let is_match = cur_path.len() == target.len()
                    && cur_path.iter().zip(target.iter()).all(|(a, b)| a == *b);
                let here = *idx;
                *idx += 1;
                if is_match {
                    let res = Some(here);
                    cur_path.pop();
                    return res;
                }
                if e.is_dir() && e.is_expanded {
                    if let Some(r) = search(&e.children, target, idx, cur_path) {
                        cur_path.pop();
                        return Some(r);
                    }
                }
                cur_path.pop();
            }
            None
        }
        search(&self.entries, names, &mut idx, &mut Vec::new())
    }
}
