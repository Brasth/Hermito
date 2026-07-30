//! Project tree: recursive collapsible model + cancellable off-thread scans
//! using ignore filtering. Dirs-before-files sort. Explicit refresh by caller.
//! No filesystem work on UI thread. Entries resolve safe paths under root.

pub mod fs;
pub mod tree;

pub use fs::{load_project_file, scan_project, ProjectFileLoadResult, ProjectScanResult};
pub use tree::{EntryKind, ProjectEntry, ProjectTree};
