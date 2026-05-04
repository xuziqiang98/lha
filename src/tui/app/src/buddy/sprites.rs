use adam_agent::config::types::BuddyEye;
use adam_agent::config::types::BuddyHat;
use adam_agent::config::types::BuddySpecies;

const FRAMES: usize = 3;

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
    if let Some(hat_line) = hat_line(hat) {
        lines[0] = hat_line.to_string();
    }
    lines
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
        BuddyHat::Propeller => Some("    -|-     "),
        BuddyHat::Halo => Some("   .----.   "),
        BuddyHat::Wizard => Some("    /\\      "),
        BuddyHat::Beanie => Some("   ,----.   "),
        BuddyHat::TinyDuck => Some("    __      "),
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
        "   /\\\\  /\\\\   ",
        "  (({E})({E}))  ",
        "  (  ><  )  ",
        "   `----`   ",
    ],
    [
        "            ",
        "   /\\\\  /\\\\   ",
        "  (({E})({E}))  ",
        "  (  ><  )  ",
        "   .----.   ",
    ],
    [
        "            ",
        "   /\\\\  /\\\\   ",
        "  (({E})(-))  ",
        "  (  ><  )  ",
        "   `----`   ",
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
