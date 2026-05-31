use lha_agent::config::types::BuddyRarity;
use ratatui::style::Color;

pub(crate) fn rarity_color(rarity: BuddyRarity) -> Color {
    match rarity {
        BuddyRarity::Common => Color::Cyan,
        BuddyRarity::Uncommon => Color::Green,
        BuddyRarity::Rare => Color::Blue,
        BuddyRarity::Epic => Color::Magenta,
        BuddyRarity::Legendary => Color::Yellow,
    }
}
