use crate::product::agent::config::types::TuiBuddy;

pub(crate) const BUDDY_COMPANION_DISABLED_INSTRUCTIONS: &str = "\
<buddy_companion>
The TUI buddy companion is currently inactive. Ignore any previous buddy_companion instructions.
</buddy_companion>";

fn buddy_identity(buddy: &TuiBuddy) -> Option<(&str, String)> {
    if !buddy.enabled || buddy.muted || !buddy.observer.enabled {
        return None;
    }

    let name = buddy.name.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    let species = buddy.species.map(|species| species.to_string())?;
    Some((name, species))
}

fn personality_sentence(name: &str, personality: Option<&str>) -> Option<String> {
    let personality = personality
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(format!("{name} has a {personality} temperament."))
}

pub(crate) fn buddy_model_instructions(buddy: &TuiBuddy) -> Option<String> {
    let (name, species) = buddy_identity(buddy)?;
    let personality = personality_sentence(name, buddy.personality.as_deref());
    let personality = personality
        .map(|value| format!(" {value}"))
        .unwrap_or_default();
    Some(format!(
        "<buddy_companion>\n\
This is the current TUI buddy companion context and replaces any previous buddy_companion context.\n\
A small {species} named {name} sits beside the user's input box and occasionally comments in a speech bubble.{personality}\n\n\
You are not {name}; it is a separate UI companion. When the user addresses {name} directly, stay out of the way: respond in one line or less, or answer only the part meant for you. Do not narrate what {name} might say; the bubble handles that.\n\
</buddy_companion>"
    ))
}

#[cfg(test)]
mod tests {
    use crate::product::agent::config::types::BuddyObserverConfig;
    use crate::product::agent::config::types::BuddySpecies;
    use crate::product::agent::config::types::TuiBuddy;

    use super::*;

    fn buddy() -> TuiBuddy {
        TuiBuddy {
            enabled: true,
            muted: false,
            name: Some("Byte".to_string()),
            species: Some(BuddySpecies::Duck),
            personality: Some("quiet optimizer".to_string()),
            observer: BuddyObserverConfig {
                enabled: true,
                ..BuddyObserverConfig::default()
            },
            ..TuiBuddy::default()
        }
    }

    #[test]
    fn model_instructions_are_omitted_when_talk_is_off() {
        let buddy = TuiBuddy {
            observer: BuddyObserverConfig {
                enabled: false,
                ..BuddyObserverConfig::default()
            },
            ..buddy()
        };

        assert_eq!(buddy_model_instructions(&buddy), None);
    }
}
