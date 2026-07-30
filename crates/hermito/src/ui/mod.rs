pub mod authority_path;
pub mod editor;
pub mod project_tree;
pub mod status_bar;
pub mod terminal_pane;
pub mod theme;
pub mod tool_stripe;
pub mod tool_window;
pub mod toolbar;
pub mod workbench;

use crate::app::{AuthorityKind, TrustLevel};

pub(crate) const fn authority_kind_label(kind: AuthorityKind) -> &'static str {
    match kind {
        AuthorityKind::Local => "LOCAL",
        AuthorityKind::Ssh => "SSH",
        AuthorityKind::DevContainer => "DEVCONTAINER",
    }
}

pub(crate) const fn trust_label(trust: TrustLevel) -> &'static str {
    match trust {
        TrustLevel::Trusted => "TRUSTED",
        TrustLevel::InspectOnly => "INSPECT ONLY",
    }
}
