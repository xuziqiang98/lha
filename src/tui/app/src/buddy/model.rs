use adam_agent::config::types::BuddySpecies;
use adam_agent::config::types::TuiBuddy;

pub(crate) const DEFAULT_BUDDY_NAME: &str = "Byte";
pub(crate) const DEFAULT_BUDDY_SPECIES: BuddySpecies = BuddySpecies::Duck;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Buddy {
    pub(crate) name: String,
    pub(crate) species: BuddySpecies,
}

impl Buddy {
    pub(crate) fn from_config(config: &TuiBuddy) -> Option<Self> {
        let name = config.name.as_ref()?.trim();
        if name.is_empty() {
            return None;
        }
        Some(Self {
            name: name.to_string(),
            species: config.species.unwrap_or(DEFAULT_BUDDY_SPECIES),
        })
    }
}

pub(crate) fn validate_buddy_name(name: &str) -> Result<String, &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Buddy name cannot be empty.");
    }
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return Err("Buddy name must fit on one line.");
    }
    if trimmed.chars().count() > 24 {
        return Err("Buddy name must be 24 characters or fewer.");
    }
    Ok(trimmed.to_string())
}
