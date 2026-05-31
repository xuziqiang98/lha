use lha_agent::config::types::BuddyEye;
use lha_agent::config::types::BuddyHat;
use lha_agent::config::types::BuddyRarity;
use lha_agent::config::types::BuddySpecies;
use lha_protocol::config_types::IdentityKind;
use rand::Rng;

pub(crate) const DEFAULT_BUDDY_RARITY: BuddyRarity = BuddyRarity::Common;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Buddy {
    pub(crate) name: String,
    pub(crate) species: BuddySpecies,
    pub(crate) eye: BuddyEye,
    pub(crate) hat: BuddyHat,
    pub(crate) rarity: BuddyRarity,
    pub(crate) shiny: bool,
    pub(crate) personality: String,
    pub(crate) stats: BuddyStats,
    pub(crate) identity_kind: IdentityKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BuddyStats {
    pub(crate) debugging: u8,
    pub(crate) patience: u8,
    pub(crate) chaos: u8,
    pub(crate) wisdom: u8,
    pub(crate) snark: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatName {
    Debugging,
    Patience,
    Chaos,
    Wisdom,
    Snark,
}

const SPECIES: [BuddySpecies; 18] = [
    BuddySpecies::Duck,
    BuddySpecies::Goose,
    BuddySpecies::Blob,
    BuddySpecies::Cat,
    BuddySpecies::Dragon,
    BuddySpecies::Octopus,
    BuddySpecies::Owl,
    BuddySpecies::Penguin,
    BuddySpecies::Turtle,
    BuddySpecies::Snail,
    BuddySpecies::Ghost,
    BuddySpecies::Axolotl,
    BuddySpecies::Capybara,
    BuddySpecies::Cactus,
    BuddySpecies::Robot,
    BuddySpecies::Rabbit,
    BuddySpecies::Mushroom,
    BuddySpecies::Chonk,
];

const EYES: [BuddyEye; 6] = [
    BuddyEye::Dot,
    BuddyEye::Sparkle,
    BuddyEye::Cross,
    BuddyEye::Circle,
    BuddyEye::At,
    BuddyEye::Degree,
];

const HATS: [BuddyHat; 8] = [
    BuddyHat::None,
    BuddyHat::Crown,
    BuddyHat::TopHat,
    BuddyHat::Propeller,
    BuddyHat::Halo,
    BuddyHat::Wizard,
    BuddyHat::Beanie,
    BuddyHat::TinyDuck,
];

const NAMES: [&str; 24] = [
    "Byte", "Nib", "Patch", "Miso", "Dot", "Fennel", "Quill", "Pip", "Kilo", "Fig", "Bram", "Nova",
    "Tock", "Mochi", "Rune", "Pixel", "Tilde", "Bento", "Zed", "Luma", "Crumb", "Echo", "Mallow",
    "Orbit",
];

const PERSONALITIES: [&str; 18] = [
    "patient debugger",
    "chaotic note-taker",
    "quiet optimizer",
    "tiny reviewer",
    "terminal philosopher",
    "lint whisperer",
    "spec gardener",
    "branch cartographer",
    "diff enthusiast",
    "calm incident scribe",
    "sparkly build watcher",
    "sleepy test runner",
    "snack-powered planner",
    "curious stack climber",
    "careful refactor buddy",
    "deadline weather vane",
    "context hoarder",
    "polite chaos engine",
];

const STAT_NAMES: [StatName; 5] = [
    StatName::Debugging,
    StatName::Patience,
    StatName::Chaos,
    StatName::Wisdom,
    StatName::Snark,
];

const RARITY_WEIGHTS: [(BuddyRarity, u8); 5] = [
    (BuddyRarity::Common, 45),
    (BuddyRarity::Uncommon, 28),
    (BuddyRarity::Rare, 15),
    (BuddyRarity::Epic, 8),
    (BuddyRarity::Legendary, 4),
];
const SHINY_PROBABILITY: f64 = 0.10;

pub(crate) fn generate_buddy(identity_kind: IdentityKind, rng: &mut impl Rng) -> Buddy {
    let rarity = roll_rarity(rng);
    let species = pick(rng, &SPECIES);
    let eye = pick(rng, &EYES);
    let hat = if rarity == BuddyRarity::Common {
        BuddyHat::None
    } else {
        pick(rng, &HATS)
    };
    let shiny = rng.random_bool(SHINY_PROBABILITY);
    let stats = roll_stats(rng, rarity);

    Buddy {
        name: pick(rng, &NAMES).to_string(),
        species,
        eye,
        hat,
        rarity,
        shiny,
        personality: pick(rng, &PERSONALITIES).to_string(),
        stats,
        identity_kind,
    }
}

pub(crate) fn rarity_stars(rarity: BuddyRarity) -> &'static str {
    match rarity {
        BuddyRarity::Common => "★",
        BuddyRarity::Uncommon => "★★",
        BuddyRarity::Rare => "★★★",
        BuddyRarity::Epic => "★★★★",
        BuddyRarity::Legendary => "★★★★★",
    }
}

fn pick<T: Copy>(rng: &mut impl Rng, values: &[T]) -> T {
    values[rng.random_range(0..values.len())]
}

fn roll_rarity(rng: &mut impl Rng) -> BuddyRarity {
    let mut roll = rng.random_range(0..100);
    for (rarity, weight) in RARITY_WEIGHTS {
        if roll < weight {
            return rarity;
        }
        roll -= weight;
    }
    BuddyRarity::Common
}

fn rarity_floor(rarity: BuddyRarity) -> i16 {
    match rarity {
        BuddyRarity::Common => 5,
        BuddyRarity::Uncommon => 15,
        BuddyRarity::Rare => 25,
        BuddyRarity::Epic => 35,
        BuddyRarity::Legendary => 50,
    }
}

fn roll_stats(rng: &mut impl Rng, rarity: BuddyRarity) -> BuddyStats {
    let floor = rarity_floor(rarity);
    let peak = pick(rng, &STAT_NAMES);
    let mut dump = pick(rng, &STAT_NAMES);
    while dump == peak {
        dump = pick(rng, &STAT_NAMES);
    }

    let mut stats = BuddyStats {
        debugging: 0,
        patience: 0,
        chaos: 0,
        wisdom: 0,
        snark: 0,
    };
    for name in STAT_NAMES {
        let value = if name == peak {
            (floor + 50 + rng.random_range(0..30)).min(100)
        } else if name == dump {
            (floor - 10 + rng.random_range(0..15)).max(1)
        } else {
            floor + rng.random_range(0..40)
        } as u8;
        match name {
            StatName::Debugging => stats.debugging = value,
            StatName::Patience => stats.patience = value,
            StatName::Chaos => stats.chaos = value,
            StatName::Wisdom => stats.wisdom = value,
            StatName::Snark => stats.snark = value,
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use rand::SeedableRng;
    use rand::rngs::SmallRng;

    use super::*;

    #[test]
    fn generated_buddy_has_valid_stats() {
        let mut rng = SmallRng::seed_from_u64(1);
        let buddy = generate_buddy(IdentityKind::Nobody, &mut rng);
        let stats = [
            buddy.stats.debugging,
            buddy.stats.patience,
            buddy.stats.chaos,
            buddy.stats.wisdom,
            buddy.stats.snark,
        ];

        assert!(stats.iter().all(|value| (1..=100).contains(value)));
        assert_eq!(buddy.identity_kind, IdentityKind::Nobody);
    }

    #[test]
    fn common_buddy_has_no_hat() {
        let mut found_common = false;
        for seed in 0..100 {
            let mut rng = SmallRng::seed_from_u64(seed);
            let buddy = generate_buddy(IdentityKind::Planner, &mut rng);
            if buddy.rarity == BuddyRarity::Common {
                found_common = true;
                assert_eq!(buddy.hat, BuddyHat::None);
            }
        }
        assert!(found_common);
    }

    #[test]
    fn non_common_hat_pool_includes_none() {
        assert_eq!(
            HATS,
            [
                BuddyHat::None,
                BuddyHat::Crown,
                BuddyHat::TopHat,
                BuddyHat::Propeller,
                BuddyHat::Halo,
                BuddyHat::Wizard,
                BuddyHat::Beanie,
                BuddyHat::TinyDuck,
            ]
        );
    }

    #[test]
    fn rarity_weights_match_expected_distribution() {
        assert_eq!(
            RARITY_WEIGHTS,
            [
                (BuddyRarity::Common, 45),
                (BuddyRarity::Uncommon, 28),
                (BuddyRarity::Rare, 15),
                (BuddyRarity::Epic, 8),
                (BuddyRarity::Legendary, 4),
            ]
        );
    }

    #[test]
    fn rarity_weights_sum_to_100() {
        let total: u8 = RARITY_WEIGHTS.iter().map(|(_, weight)| *weight).sum();

        assert_eq!(total, 100);
    }

    #[test]
    fn shiny_probability_is_ten_percent() {
        assert_eq!(SHINY_PROBABILITY, 0.10);
    }
}
