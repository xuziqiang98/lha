use adam_agent::config::types::BuddyEye;
use adam_agent::config::types::BuddyHat;
use adam_agent::config::types::BuddySpecies;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

const FRAMES: usize = 3;
pub(crate) const SPRITE_WIDTH: usize = 12;

pub(crate) fn sprite_frame_count(_species: BuddySpecies) -> usize {
    FRAMES
}

pub(crate) fn render_sprite(
    species: BuddySpecies,
    eye: BuddyEye,
    hat: BuddyHat,
    blink: bool,
    frame: usize,
) -> Vec<String> {
    let body = frames_for(species)[frame % sprite_frame_count(species)];
    let mut lines = body
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>();
    let eye = if blink { "-" } else { eye_glyph(eye) };
    for line in &mut lines {
        *line = line.replace("{E}", eye);
    }
    if let Some(hat_line) = hat_line(hat)
        && let Some(first_line) = lines.first_mut()
    {
        *first_line = hat_line.to_string();
    }
    lines
        .into_iter()
        .map(|line| centered_to_width(line.trim(), SPRITE_WIDTH))
        .collect()
}

fn centered_to_width(text: &str, width: usize) -> String {
    let text = truncate_to_width(text, width);
    let text_width = UnicodeWidthStr::width(text.as_str());
    let left_pad = width.saturating_sub(text_width) / 2;
    let right_pad = width.saturating_sub(text_width + left_pad);
    format!("{}{}{}", " ".repeat(left_pad), text, " ".repeat(right_pad))
}

fn truncate_to_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut out_width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if out_width + ch_width > width {
            break;
        }
        out.push(ch);
        out_width += ch_width;
    }
    out
}

fn eye_glyph(eye: BuddyEye) -> &'static str {
    match eye {
        BuddyEye::Dot => "·",
        BuddyEye::Sparkle => "✦",
        BuddyEye::Cross => "×",
        BuddyEye::Circle => "◉",
        BuddyEye::At => "@",
        BuddyEye::Degree => "°",
    }
}

fn hat_line(hat: BuddyHat) -> Option<&'static str> {
    match hat {
        BuddyHat::None => None,
        BuddyHat::Crown => Some("    _/\\_    "),
        BuddyHat::TopHat => Some("   .----.   "),
        BuddyHat::Propeller => Some("-|-"),
        BuddyHat::Halo => Some("   .----.   "),
        BuddyHat::Wizard => Some("/\\"),
        BuddyHat::Beanie => Some("   ,----.   "),
        BuddyHat::TinyDuck => Some("__"),
    }
}

fn frames_for(species: BuddySpecies) -> &'static [[&'static str; 5]; FRAMES] {
    match species {
        BuddySpecies::Duck => &DUCK,
        BuddySpecies::Cat => &CAT,
        BuddySpecies::Blob => &BLOB,
        BuddySpecies::Robot => &ROBOT,
        BuddySpecies::Turtle => &TURTLE,
        BuddySpecies::Goose => &GOOSE,
        BuddySpecies::Dragon => &DRAGON,
        BuddySpecies::Octopus => &OCTOPUS,
        BuddySpecies::Owl => &OWL,
        BuddySpecies::Penguin => &PENGUIN,
        BuddySpecies::Snail => &SNAIL,
        BuddySpecies::Ghost => &GHOST,
        BuddySpecies::Axolotl => &AXOLOTL,
        BuddySpecies::Capybara => &CAPYBARA,
        BuddySpecies::Cactus => &CACTUS,
        BuddySpecies::Rabbit => &RABBIT,
        BuddySpecies::Mushroom => &MUSHROOM,
        BuddySpecies::Chonk => &CHONK,
    }
}

const DUCK: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "    __      ",
        "  <({E} )___  ",
        "   (  ._>   ",
        "    `--`    ",
    ],
    [
        "            ",
        "    __      ",
        "  <({E} )___  ",
        "   (  ._>   ",
        "    `--`~   ",
    ],
    [
        "            ",
        "    __      ",
        "  <({E} )___  ",
        "   (  .__>  ",
        "    `--`    ",
    ],
];
const GOOSE: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "     ({E}>    ",
        "     ||     ",
        "   _(__)_   ",
        "    ^^^^    ",
    ],
    [
        "            ",
        "    ({E}>     ",
        "     ||     ",
        "   _(__)_   ",
        "    ^^^^    ",
    ],
    [
        "            ",
        "     ({E}>>   ",
        "     ||     ",
        "   _(__)_   ",
        "    ^^^^    ",
    ],
];
const BLOB: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   .----.   ",
        "  ( {E}  {E} )  ",
        "  (      )  ",
        "   `----`   ",
    ],
    [
        "            ",
        "  .------.  ",
        " (  {E}  {E}  ) ",
        " (        ) ",
        "  `------`  ",
    ],
    [
        "            ",
        "    .--.    ",
        "   ({E}  {E})   ",
        "   (    )   ",
        "    `--`    ",
    ],
];
const CAT: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   /\\_/\\\\    ",
        "  ( {E}   {E})  ",
        "  (  w  )   ",
        "  (\")_(\")   ",
    ],
    [
        "            ",
        "   /\\_/\\\\    ",
        "  ( {E}   {E})  ",
        "  (  w  )   ",
        "  (\")_(\")~  ",
    ],
    [
        "            ",
        "   /\\-/\\\\    ",
        "  ( {E}   {E})  ",
        "  (  w  )   ",
        "  (\")_(\")   ",
    ],
];
const DRAGON: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  /^\\\\  /^\\\\  ",
        " <  {E}  {E}  > ",
        " (   ~~   ) ",
        "  `-vvvv-`  ",
    ],
    [
        "            ",
        "  /^\\\\  /^\\\\  ",
        " <  {E}  {E}  > ",
        " (        ) ",
        "  `-vvvv-`  ",
    ],
    [
        "   ~    ~   ",
        "  /^\\\\  /^\\\\  ",
        " <  {E}  {E}  > ",
        " (   ~~   ) ",
        "  `-vvvv-`  ",
    ],
];
const OCTOPUS: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   .----.   ",
        "  ( {E}  {E} )  ",
        "  (______)  ",
        "  /\\/\\/\\/\\\\  ",
    ],
    [
        "            ",
        "   .----.   ",
        "  ( {E}  {E} )  ",
        "  (______)  ",
        "  \\/\\/\\/\\/  ",
    ],
    [
        "     o      ",
        "   .----.   ",
        "  ( {E}  {E} )  ",
        "  (______)  ",
        "  /\\/\\/\\/\\\\  ",
    ],
];
const OWL: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  /\\  /\\  ",
        " (({E})({E})) ",
        " (  ><  ) ",
        "  `----`  ",
    ],
    [
        "            ",
        "  /\\  /\\  ",
        " (({E})({E})) ",
        " (  ><  ) ",
        "  .----.  ",
    ],
    [
        "            ",
        "  /\\  /\\  ",
        " (({E})({E})) ",
        " (  ><  ) ",
        "  `----`  ",
    ],
];
const PENGUIN: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  .---.     ",
        "  ({E}>{E})     ",
        " /(   )\\\\    ",
        "  `---`     ",
    ],
    [
        "            ",
        "  .---.     ",
        "  ({E}>{E})     ",
        " |(   )|    ",
        "  `---`     ",
    ],
    [
        "  .---.     ",
        "  ({E}>{E})     ",
        " /(   )\\\\    ",
        "  `---`     ",
        "   ~ ~      ",
    ],
];
const TURTLE: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   _,--._   ",
        "  ( {E}  {E} )  ",
        " /[______]\\\\ ",
        "  ``    ``  ",
    ],
    [
        "            ",
        "   _,--._   ",
        "  ( {E}  {E} )  ",
        " /[______]\\\\ ",
        "   ``  ``   ",
    ],
    [
        "            ",
        "   _,--._   ",
        "  ( {E}  {E} )  ",
        " /[======]\\\\ ",
        "  ``    ``  ",
    ],
];
const SNAIL: [[&str; 5]; FRAMES] = [
    [
        "            ",
        " {E}    .--.  ",
        "  \\\\  ( @ )  ",
        "   \\\\_`--`   ",
        "  ~~~~~~~   ",
    ],
    [
        "            ",
        "  {E}   .--.  ",
        "  |  ( @ )  ",
        "   \\\\_`--`   ",
        "  ~~~~~~~   ",
    ],
    [
        "            ",
        " {E}    .--.  ",
        "  \\\\  ( @  ) ",
        "   \\\\_`--`   ",
        "   ~~~~~~   ",
    ],
];
const GHOST: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   .----.   ",
        "  ( {E}  {E} )  ",
        "  |  ..  |  ",
        "  `~~~~~~`  ",
    ],
    [
        "            ",
        "   .----.   ",
        "  ( {E}  {E} )  ",
        "  |      |  ",
        "  `~~~~~~`  ",
    ],
    [
        "     .      ",
        "   .----.   ",
        "  ( {E}  {E} )  ",
        "  |  ..  |  ",
        "  `~~~~~~`  ",
    ],
];
const AXOLOTL: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  ~=\\__/=~  ",
        "  ( {E}  {E} )  ",
        "   /|~~|\\\\   ",
        "    /  \\\\    ",
    ],
    [
        "            ",
        "  ~=\\__/=~  ",
        "  ( {E}  {E} )  ",
        "   /|==|\\\\   ",
        "    /  \\\\    ",
    ],
    [
        "   ~   ~    ",
        "  ~=\\__/=~  ",
        "  ( {E}  {E} )  ",
        "   /|~~|\\\\   ",
        "    /  \\\\    ",
    ],
];
const CAPYBARA: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   .----.   ",
        " _( {E}  {E} )_ ",
        "(__________) ",
        "  /_/  \\\\_\\\\  ",
    ],
    [
        "            ",
        "   .----.   ",
        " _( {E}  {E} )_ ",
        "(__________)~",
        "  /_/  \\\\_\\\\  ",
    ],
    [
        "            ",
        "   .----.   ",
        " _( {E}  {E} )_ ",
        "(_________)  ",
        "  /_/  \\\\_\\\\  ",
    ],
];
const CACTUS: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "    _ _     ",
        "  _| {E}|_   ",
        " |  ___  |  ",
        "    |_|     ",
    ],
    [
        "            ",
        "    _ _     ",
        "  _| {E}|_   ",
        " | |_ _| |  ",
        "    |_|     ",
    ],
    [
        "     *      ",
        "    _ _     ",
        "  _| {E}|_   ",
        " |  ___  |  ",
        "    |_|     ",
    ],
];
const ROBOT: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  .------.  ",
        "  | {E}  {E} |  ",
        "  | [__] |  ",
        "   /|_|\\\\   ",
    ],
    [
        "            ",
        "  .------.  ",
        "  | {E}  {E} |  ",
        "  | [__] |  ",
        "  _/|_|\\\\_  ",
    ],
    [
        "   .----.   ",
        "  .------.  ",
        "  | {E}  {E} |  ",
        "  | [__] |  ",
        "   /|_|\\\\   ",
    ],
];
const RABBIT: [[&str; 5]; FRAMES] = [
    [
        "  ()  ()    ",
        "  ( {E}  {E} )   ",
        "  /  --  \\\\  ",
        " (________) ",
        "   /    \\\\   ",
    ],
    [
        "  ()  ()    ",
        "  ( {E}  {E} )   ",
        "  /  --  \\\\  ",
        " (________)~",
        "   /    \\\\   ",
    ],
    [
        "  /\\  /\\    ",
        "  ( {E}  {E} )   ",
        "  /  --  \\\\  ",
        " (________) ",
        "   /    \\\\   ",
    ],
];
const MUSHROOM: [[&str; 5]; FRAMES] = [
    [
        "   .----.   ",
        " /( {E}  {E} )\\\\ ",
        " `-.____.-`  ",
        "    ||||     ",
        "    ||||     ",
    ],
    [
        "   .----.   ",
        " /( {E}  {E} )\\\\ ",
        " `-.____.-`~ ",
        "    ||||     ",
        "    ||||     ",
    ],
    [
        "    .--.    ",
        " /( {E}  {E} )\\\\ ",
        " `-.____.-`  ",
        "    ||||     ",
        "    ||||     ",
    ],
];
const CHONK: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  .------.  ",
        " ( {E}  .. {E} ) ",
        "(__________) ",
        "  /_/  \\\\_\\\\  ",
    ],
    [
        "            ",
        "  .------.  ",
        " ( {E}  .. {E} ) ",
        "(__________)~",
        "  /_/  \\\\_\\\\  ",
    ],
    [
        "            ",
        "  .------.  ",
        " ( {E}  -- {E} ) ",
        "(__________) ",
        "  /_/  \\\\_\\\\  ",
    ],
];

#[cfg(test)]
mod tests {
    use super::*;

    const SPECIES: [BuddySpecies; 18] = [
        BuddySpecies::Duck,
        BuddySpecies::Cat,
        BuddySpecies::Blob,
        BuddySpecies::Robot,
        BuddySpecies::Turtle,
        BuddySpecies::Goose,
        BuddySpecies::Dragon,
        BuddySpecies::Octopus,
        BuddySpecies::Owl,
        BuddySpecies::Penguin,
        BuddySpecies::Snail,
        BuddySpecies::Ghost,
        BuddySpecies::Axolotl,
        BuddySpecies::Capybara,
        BuddySpecies::Cactus,
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

    #[test]
    fn rendered_sprite_lines_have_stable_display_width() {
        for species in SPECIES {
            for eye in EYES {
                for hat in HATS {
                    for blink in [false, true] {
                        for frame in 0..sprite_frame_count(species) {
                            let sprite = render_sprite(species, eye, hat, blink, frame);
                            assert_eq!(sprite.len(), 5);
                            for line in sprite {
                                assert_eq!(
                                    UnicodeWidthStr::width(line.as_str()),
                                    SPRITE_WIDTH,
                                    "species={species:?} eye={eye:?} hat={hat:?} blink={blink} frame={frame} line={line:?}",
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn owl_sprite_is_symmetric_across_frames() {
        for frame in 0..sprite_frame_count(BuddySpecies::Owl) {
            let sprite = render_sprite(
                BuddySpecies::Owl,
                BuddyEye::Degree,
                BuddyHat::None,
                false,
                frame,
            );
            assert_eq!(sprite[1], "   /\\  /\\   ");
            assert_eq!(sprite[2], "  ((°)(°))  ");
            assert_eq!(sprite[3], "  (  ><  )  ");
        }
    }

    #[test]
    fn narrow_hats_are_centered_on_the_sprite_axis() {
        assert_eq!(
            render_sprite(
                BuddySpecies::Duck,
                BuddyEye::Degree,
                BuddyHat::Propeller,
                false,
                0,
            )[0],
            "    -|-     "
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Duck,
                BuddyEye::Degree,
                BuddyHat::Wizard,
                false,
                0,
            )[0],
            "     /\\     "
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Duck,
                BuddyEye::Degree,
                BuddyHat::TinyDuck,
                false,
                0,
            )[0],
            "     __     "
        );
    }
}
