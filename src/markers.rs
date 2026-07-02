use std::path::Path;

pub(crate) const RELAY_COMMAND_SKILL_MARKER: &str = ".relay-command";

pub(crate) fn is_relay_generated_command_skill(dir: &Path) -> bool {
    dir.join(RELAY_COMMAND_SKILL_MARKER).is_file()
}
