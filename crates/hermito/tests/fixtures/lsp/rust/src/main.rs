mod shared;

use shared::shared_label;

struct Profile {
    name: String,
}

fn main() {
    let profile = Profile {
        name: String::from("Ada"),
    };
    let completion = profile.name.as_str();
    let hover_target = shared_label();
    let local_rename_target = "local";
    let definition_use = local_rename_target;
    let diagnostic = profile.missing;

    println!("{completion} {hover_target} {definition_use} {diagnostic}");
}
