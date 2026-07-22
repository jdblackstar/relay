use std::io;

pub(crate) const SCOPED_SKILL_SYNC_VERSION: u32 = 1;
const CAPABILITIES_JSON: &str = r#"{"schema_version":1,"capabilities":{"skills.sync.scoped":1}}"#;

#[cfg_attr(any(test, coverage), allow(dead_code))]
pub(crate) fn print(json: bool) -> io::Result<()> {
    if json {
        println!("{CAPABILITIES_JSON}");
    } else {
        println!("skills.sync.scoped={SCOPED_SKILL_SYNC_VERSION}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_json_has_versioned_scoped_skill_sync() {
        assert_eq!(SCOPED_SKILL_SYNC_VERSION, 1);
        assert_eq!(
            CAPABILITIES_JSON,
            r#"{"schema_version":1,"capabilities":{"skills.sync.scoped":1}}"#
        );
    }
}
