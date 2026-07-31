from shared import shared_label


class Profile:
    name: str


profile = Profile()
profile.name = "Ada"
completion = profile.name
hover_target = shared_label
local_rename_target = "local"
definition_use = local_rename_target
diagnostic = profile.missing

print(completion, hover_target, definition_use, diagnostic)
