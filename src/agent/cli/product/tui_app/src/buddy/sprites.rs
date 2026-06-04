use crate::product::agent::config::types::BuddyEye;
use crate::product::agent::config::types::BuddyHat;
use crate::product::agent::config::types::BuddySpecies;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

const FRAMES: usize = 3;
const BODY_LINES: u16 = 5;
pub(crate) const SPRITE_WIDTH: usize = 12;

pub(crate) fn sprite_frame_count(_species: BuddySpecies) -> usize {
    FRAMES
}

pub(crate) fn rendered_sprite_height(species: BuddySpecies, hat: BuddyHat) -> u16 {
    let frames = frames_for(species);
    if hat != BuddyHat::None || frames.iter().any(|frame| !frame[0].trim().is_empty()) {
        BODY_LINES
    } else {
        BODY_LINES.saturating_sub(1)
    }
}

pub(crate) fn render_sprite(
    species: BuddySpecies,
    eye: BuddyEye,
    hat: BuddyHat,
    blink: bool,
    frame: usize,
) -> Vec<String> {
    let frames = frames_for(species);
    let body = frames[frame % sprite_frame_count(species)];
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
        && first_line.trim().is_empty()
    {
        *first_line = hat_line.to_string();
    }
    if lines.first().is_some_and(|line| line.trim().is_empty())
        && frames.iter().all(|frame| frame[0].trim().is_empty())
    {
        lines.remove(0);
    }
    lines
        .into_iter()
        .map(|line| normalize_to_width(&line, SPRITE_WIDTH))
        .collect()
}

fn normalize_to_width(text: &str, width: usize) -> String {
    let text = truncate_to_width(text, width);
    let text_width = UnicodeWidthStr::width(text.as_str());
    format!("{}{}", text, " ".repeat(width.saturating_sub(text_width)))
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
        BuddyHat::Crown => Some("   \\^^^/    "),
        BuddyHat::TopHat => Some("   [___]    "),
        BuddyHat::Propeller => Some("    -+-     "),
        BuddyHat::Halo => Some("   (   )    "),
        BuddyHat::Wizard => Some("    /^\\     "),
        BuddyHat::Beanie => Some("   (___)    "),
        BuddyHat::TinyDuck => Some("    ,>      "),
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
        "    `--´    ",
    ],
    [
        "            ",
        "    __      ",
        "  <({E} )___  ",
        "   (  ._>   ",
        "    `--´~   ",
    ],
    [
        "            ",
        "    __      ",
        "  <({E} )___  ",
        "   (  .__>  ",
        "    `--´    ",
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
        "   `----´   ",
    ],
    [
        "            ",
        "  .------.  ",
        " (  {E}  {E}  ) ",
        " (        ) ",
        "  `------´  ",
    ],
    [
        "            ",
        "    .--.    ",
        "   ({E}  {E})   ",
        "   (    )   ",
        "    `--´    ",
    ],
];
const CAT: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   /\\_/\\    ",
        "  ( {E}   {E})  ",
        "  (  ω  )   ",
        "  (\")_(\")   ",
    ],
    [
        "            ",
        "   /\\_/\\    ",
        "  ( {E}   {E})  ",
        "  (  ω  )   ",
        "  (\")_(\")~  ",
    ],
    [
        "            ",
        "   /\\-/\\    ",
        "  ( {E}   {E})  ",
        "  (  ω  )   ",
        "  (\")_(\")   ",
    ],
];
const DRAGON: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  /^\\  /^\\  ",
        " <  {E}  {E}  > ",
        " (   ~~   ) ",
        "  `-vvvv-´  ",
    ],
    [
        "            ",
        "  /^\\  /^\\  ",
        " <  {E}  {E}  > ",
        " (        ) ",
        "  `-vvvv-´  ",
    ],
    [
        "   ~    ~   ",
        "  /^\\  /^\\  ",
        " <  {E}  {E}  > ",
        " (   ~~   ) ",
        "  `-vvvv-´  ",
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
        "   /\\  /\\   ",
        "  (({E})({E}))  ",
        "  (  ><  )  ",
        "   `----´   ",
    ],
    [
        "            ",
        "   /\\  /\\   ",
        "  (({E})({E}))  ",
        "  (  ><  )  ",
        "   .----.   ",
    ],
    [
        "            ",
        "   /\\  /\\   ",
        "  (({E})(-))  ",
        "  (  ><  )  ",
        "   `----´   ",
    ],
];
const PENGUIN: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  .---.     ",
        "  ({E}>{E})     ",
        " /(   )\\    ",
        "  `---´     ",
    ],
    [
        "            ",
        "  .---.     ",
        "  ({E}>{E})     ",
        " |(   )|    ",
        "  `---´     ",
    ],
    [
        "  .---.     ",
        "  ({E}>{E})     ",
        " /(   )\\    ",
        "  `---´     ",
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
        "  \\  ( @ )  ",
        "   \\_`--´   ",
        "  ~~~~~~~   ",
    ],
    [
        "            ",
        "  {E}   .--.  ",
        "  |  ( @ )  ",
        "   \\_`--´   ",
        "  ~~~~~~~   ",
    ],
    [
        "            ",
        " {E}    .--.  ",
        "  \\  ( @  ) ",
        "   \\_`--´   ",
        "   ~~~~~~   ",
    ],
];
const GHOST: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   .----.   ",
        "  / {E}  {E} \\  ",
        "  |      |  ",
        "  ~`~``~`~  ",
    ],
    [
        "            ",
        "   .----.   ",
        "  / {E}  {E} \\  ",
        "  |      |  ",
        "  `~`~~`~`  ",
    ],
    [
        "    ~  ~    ",
        "   .----.   ",
        "  / {E}  {E} \\  ",
        "  |      |  ",
        "  ~~`~~`~~  ",
    ],
];
const AXOLOTL: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "}~(______)~{",
        "}~({E} .. {E})~{",
        "  ( .--. )  ",
        "  (_/  \\_)  ",
    ],
    [
        "            ",
        "~}(______){~",
        "~}({E} .. {E}){~",
        "  ( .--. )  ",
        "  (_/  \\_)  ",
    ],
    [
        "            ",
        "}~(______)~{",
        "}~({E} .. {E})~{",
        "  (  --  )  ",
        "  ~_/  \\_~  ",
    ],
];
const CAPYBARA: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  n______n  ",
        " ( {E}    {E} ) ",
        " (   oo   ) ",
        "  `------´  ",
    ],
    [
        "            ",
        "  n______n  ",
        " ( {E}    {E} ) ",
        " (   Oo   ) ",
        "  `------´  ",
    ],
    [
        "    ~  ~    ",
        "  u______n  ",
        " ( {E}    {E} ) ",
        " (   oo   ) ",
        "  `------´  ",
    ],
];
const CACTUS: [[&str; 5]; FRAMES] = [
    [
        "            ",
        " n  ____  n ",
        " | |{E}  {E}| | ",
        " |_|    |_| ",
        "   |    |   ",
    ],
    [
        "            ",
        "    ____    ",
        " n |{E}  {E}| n ",
        " |_|    |_| ",
        "   |    |   ",
    ],
    [
        " n        n ",
        " |  ____  | ",
        " | |{E}  {E}| | ",
        " |_|    |_| ",
        "   |    |   ",
    ],
];
const ROBOT: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   .[||].   ",
        "  [ {E}  {E} ]  ",
        "  [ ==== ]  ",
        "  `------´  ",
    ],
    [
        "            ",
        "   .[||].   ",
        "  [ {E}  {E} ]  ",
        "  [ -==- ]  ",
        "  `------´  ",
    ],
    [
        "     *      ",
        "   .[||].   ",
        "  [ {E}  {E} ]  ",
        "  [ ==== ]  ",
        "  `------´  ",
    ],
];
const RABBIT: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "   (\\__/)   ",
        "  ( {E}  {E} )  ",
        " =(  ..  )= ",
        "  (\")__(\")  ",
    ],
    [
        "            ",
        "   (|__/)   ",
        "  ( {E}  {E} )  ",
        " =(  ..  )= ",
        "  (\")__(\")  ",
    ],
    [
        "            ",
        "   (\\__/)   ",
        "  ( {E}  {E} )  ",
        " =( .  . )= ",
        "  (\")__(\")  ",
    ],
];
const MUSHROOM: [[&str; 5]; FRAMES] = [
    [
        "            ",
        " .-o-OO-o-. ",
        "(__________)",
        "   |{E}  {E}|   ",
        "   |____|   ",
    ],
    [
        "            ",
        " .-O-oo-O-. ",
        "(__________)",
        "   |{E}  {E}|   ",
        "   |____|   ",
    ],
    [
        "   . o  .   ",
        " .-o-OO-o-. ",
        "(__________)",
        "   |{E}  {E}|   ",
        "   |____|   ",
    ],
];
const CHONK: [[&str; 5]; FRAMES] = [
    [
        "            ",
        "  /\\    /\\  ",
        " ( {E}    {E} ) ",
        " (   ..   ) ",
        "  `------´  ",
    ],
    [
        "            ",
        "  /\\    /|  ",
        " ( {E}    {E} ) ",
        " (   ..   ) ",
        "  `------´  ",
    ],
    [
        "            ",
        "  /\\    /\\  ",
        " ( {E}    {E} ) ",
        " (   ..   ) ",
        "  `------´~ ",
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
                            assert_eq!(
                                sprite.len(),
                                usize::from(rendered_sprite_height(species, hat))
                            );
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
    fn hat_only_renders_when_hat_slot_is_blank() {
        assert_eq!(
            render_sprite(
                BuddySpecies::Dragon,
                BuddyEye::Degree,
                BuddyHat::TopHat,
                false,
                0,
            )[0],
            "   [___]    "
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Dragon,
                BuddyEye::Degree,
                BuddyHat::TopHat,
                false,
                2,
            )[0],
            "   ~    ~   "
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Penguin,
                BuddyEye::Degree,
                BuddyHat::TopHat,
                false,
                0,
            )[0],
            "   [___]    "
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Penguin,
                BuddyEye::Degree,
                BuddyHat::TopHat,
                false,
                2,
            )[0],
            "  .---.     "
        );
    }

    #[test]
    fn blank_hat_slot_is_dropped_only_when_all_frames_are_blank() {
        assert_eq!(
            render_sprite(
                BuddySpecies::Duck,
                BuddyEye::Degree,
                BuddyHat::None,
                false,
                0,
            )
            .len(),
            4
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Duck,
                BuddyEye::Degree,
                BuddyHat::TopHat,
                false,
                0,
            )
            .len(),
            5
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Dragon,
                BuddyEye::Degree,
                BuddyHat::None,
                false,
                0,
            )
            .len(),
            5
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Robot,
                BuddyEye::Degree,
                BuddyHat::None,
                false,
                0,
            )
            .len(),
            5
        );
    }

    #[test]
    fn owl_sprite_matches_claude_code_frames() {
        let frames = (0..sprite_frame_count(BuddySpecies::Owl))
            .map(|frame| {
                render_sprite(
                    BuddySpecies::Owl,
                    BuddyEye::Degree,
                    BuddyHat::None,
                    false,
                    frame,
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            frames,
            vec![
                vec![
                    "   /\\  /\\   ".to_string(),
                    "  ((°)(°))  ".to_string(),
                    "  (  ><  )  ".to_string(),
                    "   `----´   ".to_string(),
                ],
                vec![
                    "   /\\  /\\   ".to_string(),
                    "  ((°)(°))  ".to_string(),
                    "  (  ><  )  ".to_string(),
                    "   .----.   ".to_string(),
                ],
                vec![
                    "   /\\  /\\   ".to_string(),
                    "  ((°)(-))  ".to_string(),
                    "  (  ><  )  ".to_string(),
                    "   `----´   ".to_string(),
                ],
            ]
        );
    }

    #[test]
    fn changed_species_match_claude_code_shapes() {
        let cases = [
            (
                BuddySpecies::Cat,
                vec![
                    "   /\\_/\\    ".to_string(),
                    "  ( °   °)  ".to_string(),
                    "  (  ω  )   ".to_string(),
                    "  (\")_(\")   ".to_string(),
                ],
            ),
            (
                BuddySpecies::Ghost,
                vec![
                    "            ".to_string(),
                    "   .----.   ".to_string(),
                    "  / °  ° \\  ".to_string(),
                    "  |      |  ".to_string(),
                    "  ~`~``~`~  ".to_string(),
                ],
            ),
            (
                BuddySpecies::Axolotl,
                vec![
                    "}~(______)~{".to_string(),
                    "}~(° .. °)~{".to_string(),
                    "  ( .--. )  ".to_string(),
                    "  (_/  \\_)  ".to_string(),
                ],
            ),
            (
                BuddySpecies::Capybara,
                vec![
                    "            ".to_string(),
                    "  n______n  ".to_string(),
                    " ( °    ° ) ".to_string(),
                    " (   oo   ) ".to_string(),
                    "  `------´  ".to_string(),
                ],
            ),
            (
                BuddySpecies::Cactus,
                vec![
                    "            ".to_string(),
                    " n  ____  n ".to_string(),
                    " | |°  °| | ".to_string(),
                    " |_|    |_| ".to_string(),
                    "   |    |   ".to_string(),
                ],
            ),
            (
                BuddySpecies::Robot,
                vec![
                    "            ".to_string(),
                    "   .[||].   ".to_string(),
                    "  [ °  ° ]  ".to_string(),
                    "  [ ==== ]  ".to_string(),
                    "  `------´  ".to_string(),
                ],
            ),
            (
                BuddySpecies::Rabbit,
                vec![
                    "   (\\__/)   ".to_string(),
                    "  ( °  ° )  ".to_string(),
                    " =(  ..  )= ".to_string(),
                    "  (\")__(\")  ".to_string(),
                ],
            ),
            (
                BuddySpecies::Mushroom,
                vec![
                    "            ".to_string(),
                    " .-o-OO-o-. ".to_string(),
                    "(__________)".to_string(),
                    "   |°  °|   ".to_string(),
                    "   |____|   ".to_string(),
                ],
            ),
            (
                BuddySpecies::Chonk,
                vec![
                    "  /\\    /\\  ".to_string(),
                    " ( °    ° ) ".to_string(),
                    " (   ..   ) ".to_string(),
                    "  `------´  ".to_string(),
                ],
            ),
        ];

        for (species, expected) in cases {
            assert_eq!(
                render_sprite(species, BuddyEye::Degree, BuddyHat::None, false, 0),
                expected,
                "species={species:?}"
            );
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
            "    -+-     "
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Duck,
                BuddyEye::Degree,
                BuddyHat::Wizard,
                false,
                0,
            )[0],
            "    /^\\     "
        );
        assert_eq!(
            render_sprite(
                BuddySpecies::Duck,
                BuddyEye::Degree,
                BuddyHat::TinyDuck,
                false,
                0,
            )[0],
            "    ,>      "
        );
    }
}
