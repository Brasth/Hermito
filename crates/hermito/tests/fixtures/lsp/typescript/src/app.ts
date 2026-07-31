import { sharedLabel } from "./shared.js";

type Profile = {
  name: string;
};

const profile: Profile = { name: "Ada" };
const completion = profile.name;
const hoverTarget = sharedLabel;
const localRenameTarget = "local";
const definitionUse = localRenameTarget;
const diagnostic = profile.missing;

console.log(completion, hoverTarget, definitionUse, diagnostic);
