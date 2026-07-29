use pretty_assertions::assert_eq;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use unicode_width::UnicodeWidthStr;

use crate::product::tui_app::markdown_render::render_markdown_text;
use crate::product::tui_app::markdown_render::render_markdown_text_with_width;
use insta::assert_snapshot;

fn plain_lines(text: &Text<'_>) -> Vec<String> {
    text.lines.iter().map(plain_line).collect()
}

fn plain_line(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn display_column(line: &str, needle: &str) -> usize {
    let byte_index = line
        .find(needle)
        .unwrap_or_else(|| panic!("expected {needle:?} in {line:?}"));
    line[..byte_index].width()
}

#[test]
fn empty() {
    assert_eq!(render_markdown_text(""), Text::default());
}

#[test]
fn paragraph_single() {
    assert_eq!(
        render_markdown_text("Hello, world!"),
        Text::from("Hello, world!")
    );
}

#[test]
fn paragraph_soft_break() {
    assert_eq!(
        render_markdown_text("Hello\nWorld"),
        Text::from_iter(["Hello", "World"])
    );
}

#[test]
fn paragraph_multiple() {
    assert_eq!(
        render_markdown_text("Paragraph 1\n\nParagraph 2"),
        Text::from_iter(["Paragraph 1", "", "Paragraph 2"])
    );
}

#[test]
fn headings() {
    let md = "# Heading 1\n## Heading 2\n### Heading 3\n#### Heading 4\n##### Heading 5\n###### Heading 6\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["# ".bold().underlined(), "Heading 1".bold().underlined()]),
        Line::default(),
        Line::from_iter(["## ".bold(), "Heading 2".bold()]),
        Line::default(),
        Line::from_iter(["### ".bold().italic(), "Heading 3".bold().italic()]),
        Line::default(),
        Line::from_iter(["#### ".italic(), "Heading 4".italic()]),
        Line::default(),
        Line::from_iter(["##### ".italic(), "Heading 5".italic()]),
        Line::default(),
        Line::from_iter(["###### ".italic(), "Heading 6".italic()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_single() {
    let text = render_markdown_text("> Blockquote");
    let expected = Text::from(Line::from_iter(["> ", "Blockquote"]).green());
    assert_eq!(text, expected);
}

#[test]
fn blockquote_soft_break() {
    // Soft break via lazy continuation should render as a new line in blockquotes.
    let text = render_markdown_text("> This is a blockquote\nwith a soft break\n");
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "> This is a blockquote".to_string(),
            "> with a soft break".to_string()
        ]
    );
}

#[test]
fn blockquote_multiple_with_break() {
    let text = render_markdown_text("> Blockquote 1\n\n> Blockquote 2\n");
    let expected = Text::from_iter([
        Line::from_iter(["> ", "Blockquote 1"]).green(),
        Line::default(),
        Line::from_iter(["> ", "Blockquote 2"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_three_paragraphs_short_lines() {
    let md = "> one\n>\n> two\n>\n> three\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["> ", "one"]).green(),
        Line::from_iter(["> "]).green(),
        Line::from_iter(["> ", "two"]).green(),
        Line::from_iter(["> "]).green(),
        Line::from_iter(["> ", "three"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_nested_two_levels() {
    let md = "> Level 1\n>> Level 2\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["> ", "Level 1"]).green(),
        Line::from_iter(["> "]).green(),
        Line::from_iter(["> ", "> ", "Level 2"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_with_list_items() {
    let md = "> - item 1\n> - item 2\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["> ", "- ", "item 1"]).green(),
        Line::from_iter(["> ", "- ", "item 2"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_with_ordered_list() {
    let md = "> 1. first\n> 2. second\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(vec![
            Span::from("> "),
            "1. ".light_blue(),
            Span::from("first"),
        ])
        .green(),
        Line::from_iter(vec![
            Span::from("> "),
            "2. ".light_blue(),
            Span::from("second"),
        ])
        .green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn blockquote_list_then_nested_blockquote() {
    let md = "> - parent\n>   > child\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["> ", "- ", "parent"]).green(),
        Line::from_iter(["> ", "  ", "> ", "child"]).green(),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn list_item_with_inline_blockquote_on_same_line() {
    let md = "1. > quoted\n";
    let text = render_markdown_text(md);
    let mut lines = text.lines.iter();
    let first = lines.next().expect("one line");
    // Expect content to include the ordered marker, a space, "> ", and the text
    let s: String = first.spans.iter().map(|sp| sp.content.clone()).collect();
    assert_eq!(s, "1. > quoted");
}

#[test]
fn blockquote_surrounded_by_blank_lines() {
    let md = "foo\n\n> bar\n\nbaz\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "foo".to_string(),
            "".to_string(),
            "> bar".to_string(),
            "".to_string(),
            "baz".to_string(),
        ]
    );
}

#[test]
fn blockquote_in_ordered_list_on_next_line() {
    // Blockquote begins on a new line within an ordered list item; it should
    // render inline on the same marker line.
    let md = "1.\n   > quoted\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. > quoted".to_string()]);
}

#[test]
fn blockquote_in_unordered_list_on_next_line() {
    // Blockquote begins on a new line within an unordered list item; it should
    // render inline on the same marker line.
    let md = "-\n  > quoted\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["- > quoted".to_string()]);
}

#[test]
fn blockquote_two_paragraphs_inside_ordered_list_has_blank_line() {
    // Two blockquote paragraphs inside a list item should be separated by a blank line.
    let md = "1.\n   > para 1\n   >\n   > para 2\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "1. > para 1".to_string(),
            "   > ".to_string(),
            "   > para 2".to_string(),
        ],
        "expected blockquote content to stay aligned after list marker"
    );
}

#[test]
fn blockquote_inside_nested_list() {
    let md = "1. A\n    - B\n      > inner\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. A", "    - B", "      > inner"]);
}

#[test]
fn list_item_text_then_blockquote() {
    let md = "1. before\n   > quoted\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. before", "   > quoted"]);
}

#[test]
fn list_item_blockquote_then_text() {
    let md = "1.\n   > quoted\n   after\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. > quoted", "   > after"]);
}

#[test]
fn list_item_text_blockquote_text() {
    let md = "1. before\n   > quoted\n   after\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["1. before", "   > quoted", "   > after"]);
}

#[test]
fn blockquote_with_heading_and_paragraph() {
    let md = "> # Heading\n> paragraph text\n";
    let text = render_markdown_text(md);
    // Validate on content shape; styling is handled elsewhere
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "> # Heading".to_string(),
            "> ".to_string(),
            "> paragraph text".to_string(),
        ]
    );
}

#[test]
fn blockquote_heading_inherits_heading_style() {
    let text = render_markdown_text("> # test header\n> in blockquote\n");
    assert_eq!(
        text.lines,
        [
            Line::from_iter([
                "> ".into(),
                "# ".bold().underlined(),
                "test header".bold().underlined(),
            ])
            .green(),
            Line::from_iter(["> "]).green(),
            Line::from_iter(["> ", "in blockquote"]).green(),
        ]
    );
}

#[test]
fn blockquote_with_code_block() {
    let md = "> ```\n> code\n> ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["> code".to_string()]);
}

#[test]
fn blockquote_with_multiline_code_block() {
    let md = "> ```\n> first\n> second\n> ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["> first", "> second"]);
}

#[test]
fn nested_blockquote_with_inline_and_fenced_code() {
    /*
    let md = \"> Nested quote with code:\n\
    > > Inner quote and `inline code`\n\
    > >\n\
    > > ```\n\
    > > # fenced code inside a quote\n\
    > > echo \"hello from a quote\"\n\
    > > ```\n";
    */
    let md = r#"> Nested quote with code:
> > Inner quote and `inline code`
> >
> > ```
> > # fenced code inside a quote
> > echo "hello from a quote"
> > ```
"#;
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "> Nested quote with code:".to_string(),
            "> ".to_string(),
            "> > Inner quote and inline code".to_string(),
            "> > ".to_string(),
            "> > # fenced code inside a quote".to_string(),
            "> > echo \"hello from a quote\"".to_string(),
        ]
    );
}

#[test]
fn list_unordered_single() {
    let text = render_markdown_text("- List item 1\n");
    let expected = Text::from_iter([Line::from_iter(["- ", "List item 1"])]);
    assert_eq!(text, expected);
}

#[test]
fn list_unordered_multiple() {
    let text = render_markdown_text("- List item 1\n- List item 2\n");
    let expected = Text::from_iter([
        Line::from_iter(["- ", "List item 1"]),
        Line::from_iter(["- ", "List item 2"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn list_ordered() {
    let text = render_markdown_text("1. List item 1\n2. List item 2\n");
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "List item 1".into()]),
        Line::from_iter(["2. ".light_blue(), "List item 2".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn list_nested() {
    let text = render_markdown_text("- List item 1\n  - Nested list item 1\n");
    let expected = Text::from_iter([
        Line::from_iter(["- ", "List item 1"]),
        Line::from_iter(["    - ", "Nested list item 1"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn list_ordered_custom_start() {
    let text = render_markdown_text("3. First\n4. Second\n");
    let expected = Text::from_iter([
        Line::from_iter(["3. ".light_blue(), "First".into()]),
        Line::from_iter(["4. ".light_blue(), "Second".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn nested_unordered_in_ordered() {
    let md = "1. Outer\n    - Inner A\n    - Inner B\n2. Next\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "Outer".into()]),
        Line::from_iter(["    - ", "Inner A"]),
        Line::from_iter(["    - ", "Inner B"]),
        Line::from_iter(["2. ".light_blue(), "Next".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn nested_ordered_in_unordered() {
    let md = "- Outer\n    1. One\n    2. Two\n- Last\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["- ", "Outer"]),
        Line::from_iter(["    1. ".light_blue(), "One".into()]),
        Line::from_iter(["    2. ".light_blue(), "Two".into()]),
        Line::from_iter(["- ", "Last"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn loose_list_item_multiple_paragraphs() {
    let md = "1. First paragraph\n\n   Second paragraph of same item\n\n2. Next item\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "First paragraph".into()]),
        Line::default(),
        Line::from_iter(["   ", "Second paragraph of same item"]),
        Line::from_iter(["2. ".light_blue(), "Next item".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn tight_item_with_soft_break() {
    let md = "- item line1\n  item line2\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["- ", "item line1"]),
        Line::from_iter(["  ", "item line2"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn deeply_nested_mixed_three_levels() {
    let md = "1. A\n    - B\n        1. C\n2. D\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "A".into()]),
        Line::from_iter(["    - ", "B"]),
        Line::from_iter(["        1. ".light_blue(), "C".into()]),
        Line::from_iter(["2. ".light_blue(), "D".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn loose_items_due_to_blank_line_between_items() {
    let md = "1. First\n\n2. Second\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "First".into()]),
        Line::from_iter(["2. ".light_blue(), "Second".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn mixed_tight_then_loose_in_one_list() {
    let md = "1. Tight\n\n2.\n   Loose\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "Tight".into()]),
        Line::from_iter(["2. ".light_blue(), "Loose".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn ordered_item_with_indented_continuation_is_tight() {
    let md = "1. Foo\n   Bar\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "Foo".into()]),
        Line::from_iter(["   ", "Bar"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn inline_code() {
    let text = render_markdown_text("Example of `Inline code`");
    let expected = Line::from_iter(["Example of ".into(), "Inline code".cyan()]).into();
    assert_eq!(text, expected);
}

#[test]
fn strong() {
    assert_eq!(
        render_markdown_text("**Strong**"),
        Text::from(Line::from("Strong".bold()))
    );
}

#[test]
fn emphasis() {
    assert_eq!(
        render_markdown_text("*Emphasis*"),
        Text::from(Line::from("Emphasis".italic()))
    );
}

#[test]
fn strikethrough() {
    assert_eq!(
        render_markdown_text("~~Strikethrough~~"),
        Text::from(Line::from("Strikethrough".crossed_out()))
    );
}

#[test]
fn strong_emphasis() {
    let text = render_markdown_text("**Strong *emphasis***");
    let expected = Text::from(Line::from_iter([
        "Strong ".bold(),
        "emphasis".bold().italic(),
    ]));
    assert_eq!(text, expected);
}

#[test]
fn link() {
    let text = render_markdown_text("[Link](https://example.com)");
    let expected = Text::from(Line::from_iter([
        "Link".into(),
        " (".into(),
        "https://example.com".cyan().underlined(),
        ")".into(),
    ]));
    assert_eq!(text, expected);
}

#[test]
fn code_block_unhighlighted() {
    let text = render_markdown_text("```rust\nfn main() {}\n```\n");
    let expected = Text::from_iter([Line::from_iter(["", "fn main() {}"])]);
    assert_eq!(text, expected);
}

#[test]
fn code_block_multiple_lines_root() {
    let md = "```\nfirst\nsecond\n```\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["", "first"]),
        Line::from_iter(["", "second"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn code_block_indented() {
    let md = "    function greet() {\n      console.log(\"Hi\");\n    }\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["    ", "function greet() {"]),
        Line::from_iter(["    ", "  console.log(\"Hi\");"]),
        Line::from_iter(["    ", "}"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn horizontal_rule_renders_em_dashes() {
    let md = "Before\n\n---\n\nAfter\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["Before", "", "———", "", "After"]);
}

#[test]
fn code_block_with_inner_triple_backticks_outer_four() {
    let md = r#"````text
Here is a code block that shows another fenced block:

```md
# Inside fence
- bullet
- `inline code`
```
````
"#;
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "Here is a code block that shows another fenced block:".to_string(),
            String::new(),
            "```md".to_string(),
            "# Inside fence".to_string(),
            "- bullet".to_string(),
            "- `inline code`".to_string(),
            "```".to_string(),
        ]
    );
}

#[test]
fn code_block_inside_unordered_list_item_is_indented() {
    let md = "- Item\n\n  ```\n  code line\n  ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["- Item", "", "  code line"]);
}

#[test]
fn code_block_multiple_lines_inside_unordered_list() {
    let md = "- Item\n\n  ```\n  first\n  second\n  ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["- Item", "", "  first", "  second"]);
}

#[test]
fn code_block_inside_unordered_list_item_multiple_lines() {
    let md = "- Item\n\n  ```\n  first\n  second\n  ```\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(lines, vec!["- Item", "", "  first", "  second"]);
}

#[test]
fn markdown_render_complex_snapshot() {
    let md = r#"# H1: Markdown Streaming Test
Intro paragraph with bold **text**, italic *text*, and inline code `x=1`.
Combined bold-italic ***both*** and escaped asterisks \*literal\*.
Auto-link: <https://example.com> and reference link [ref][r1].
Link with title: [hover me](https://example.com "Example") and mailto <mailto:test@example.com>.
Image: ![alt text](https://example.com/img.png "Title")
> Blockquote level 1
>> Blockquote level 2 with `inline code`
- Unordered list item 1
  - Nested bullet with italics _inner_
- Unordered list item 2 with ~~strikethrough~~
1. Ordered item one
2. Ordered item two with sublist:
   1) Alt-numbered subitem
- [ ] Task: unchecked
- [x] Task: checked with link [home](https://example.org)
---
Table below (alignment test):
| Left | Center | Right |
|:-----|:------:|------:|
| a    |   b    |     c |
Inline HTML: <sup>sup</sup> and <sub>sub</sub>.
HTML block:
<div style="border:1px solid #ccc;padding:2px">inline block</div>
Escapes: \_underscores\_, backslash \\, ticks ``code with `backtick` inside``.
Emoji shortcodes: :sparkles: :tada: (if supported).
Hard break test (line ends with two spaces)  
Next line should be close to previous.
Footnote reference here[^1] and another[^longnote].
Horizontal rule with asterisks:
***
Fenced code block (JSON):
```json
{ "a": 1, "b": [true, false] }
```
Fenced code with tildes and triple backticks inside:
~~~markdown
To close ``` you need tildes.
~~~
Indented code block:
    for i in range(3): print(i)
Definition-like list:
Term
: Definition with `code`.
Character entities: &amp; &lt; &gt; &quot; &#39;
[^1]: This is the first footnote.
[^longnote]: A longer footnote with a link to [Rust](https://www.rust-lang.org/).
Escaped pipe in text: a \| b \| c.
URL with parentheses: [link](https://example.com/path_(with)_parens).
[r1]: https://example.com/ref "Reference link title"
"#;

    let text = render_markdown_text(md);
    // Convert to plain text lines for snapshot (ignore styles)
    let rendered = text
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.clone())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert_snapshot!(rendered);
}

#[test]
fn ordered_item_with_code_block_and_nested_bullet() {
    let md = "1. **item 1**\n\n2. **item 2**\n   ```\n   code\n   ```\n   - `PROCESS_START` (a `OnceLock<Instant>`) keeps the start time for the entire process.\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "1. item 1".to_string(),
            "2. item 2".to_string(),
            String::new(),
            "   code".to_string(),
            "    - PROCESS_START (a OnceLock<Instant>) keeps the start time for the entire process.".to_string(),
        ]
    );
}

#[test]
fn nested_five_levels_mixed_lists() {
    let md = "1. First\n   - Second level\n     1. Third level (ordered)\n        - Fourth level (bullet)\n          - Fifth level to test indent consistency\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "First".into()]),
        Line::from_iter(["    - ", "Second level"]),
        Line::from_iter(["        1. ".light_blue(), "Third level (ordered)".into()]),
        Line::from_iter(["            - ", "Fourth level (bullet)"]),
        Line::from_iter([
            "                - ",
            "Fifth level to test indent consistency",
        ]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn html_comment_inline_is_hidden() {
    let text = render_markdown_text("Hello <!-- -->world");
    let expected: Text = Line::from_iter(["Hello ", "world"]).into();
    assert_eq!(text, expected);
}

#[test]
fn html_comment_block_is_hidden() {
    let text = render_markdown_text("<!-- hidden -->\nVisible");
    let rendered = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(!rendered.contains("<!-- hidden -->"));
    assert!(rendered.contains("Visible"));
}

#[test]
fn html_leading_comment_before_visible_markdown_is_stripped() {
    let text = render_markdown_text("<!-- -->**Planning**");
    let expected: Text = Line::from("**Planning**").into();
    assert_eq!(text, expected);
}

#[test]
fn html_inline_is_verbatim() {
    let md = "Hello <span>world</span>!";
    let text = render_markdown_text(md);
    let expected: Text = Line::from_iter(["Hello ", "<span>", "world", "</span>", "!"]).into();
    assert_eq!(text, expected);
}

#[test]
fn html_block_is_verbatim_multiline() {
    let md = "<div>\n  <span>hi</span>\n</div>\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["<div>"]),
        Line::from_iter(["  <span>hi</span>"]),
        Line::from_iter(["</div>"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn html_in_tight_ordered_item_soft_breaks_with_space() {
    let md = "1. Foo\n   <i>Bar</i>\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "Foo".into()]),
        Line::from_iter(["   ", "<i>", "Bar", "</i>"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn html_continuation_paragraph_in_unordered_item_indented() {
    let md = "- Item\n\n  <em>continued</em>\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["- ", "Item"]),
        Line::default(),
        Line::from_iter(["  ", "<em>", "continued", "</em>"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn unordered_item_continuation_paragraph_is_indented() {
    let md = "- Intro\n\n  Continuation paragraph line 1\n  Continuation paragraph line 2\n";
    let text = render_markdown_text(md);
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.clone())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            "- Intro".to_string(),
            String::new(),
            "  Continuation paragraph line 1".to_string(),
            "  Continuation paragraph line 2".to_string(),
        ]
    );
}

#[test]
fn ordered_item_continuation_paragraph_is_indented() {
    let md = "1. Intro\n\n   More details about intro\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "Intro".into()]),
        Line::default(),
        Line::from_iter(["   ", "More details about intro"]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn nested_item_continuation_paragraph_is_indented() {
    let md = "1. A\n    - B\n\n      Continuation for B\n2. C\n";
    let text = render_markdown_text(md);
    let expected = Text::from_iter([
        Line::from_iter(["1. ".light_blue(), "A".into()]),
        Line::from_iter(["    - ", "B"]),
        Line::default(),
        Line::from_iter(["      ", "Continuation for B"]),
        Line::from_iter(["2. ".light_blue(), "C".into()]),
    ]);
    assert_eq!(text, expected);
}

#[test]
fn markdown_table_grid_snapshot() {
    let md = r#"| Left | Center | Right |
| :--- | :----: | ----: |
| alpha | 12 | ready |
| 中文 | 7 | 完成 |
"#;
    let text = render_markdown_text_with_width(md, Some(80));

    assert_snapshot!("markdown_table_grid", plain_lines(&text).join("\n"));
}

#[test]
fn markdown_table_long_path_width_allocation_snapshot() {
    let md = r#"| Unit | Files | Adds | Notes |
| --- | --- | ---: | --- |
| Suggestion engine | /Users/example/lha/src/agent/runtime/next_prompt_suggestion_tests.rs:104 | 704 | Sampling workflow remains readable while the path wraps first. |
| Context isolation | /Users/example/lha/src/core/context/contextual_user_message_tests.rs:88 | 54 | Ordinary prose keeps a useful width. |
"#;
    let text = render_markdown_text_with_width(md, Some(72));

    assert_snapshot!(
        "markdown_table_long_path",
        plain_lines(&text).join("\n")
    );
}

#[test]
fn markdown_table_narrow_key_value_snapshot() {
    let md = r#"| Name | Owner | Status | Description |
| --- | --- | --- | --- |
| renderer | tui | ready | Preserves every value when the terminal is extremely narrow. |
| replay | history | done | Recomputes the layout from the original Markdown source. |
"#;
    let text = render_markdown_text_with_width(md, Some(22));

    assert_snapshot!(
        "markdown_table_narrow_key_value",
        plain_lines(&text).join("\n")
    );
}

#[test]
fn markdown_table_spillover_snapshot() {
    let md = r#"| Name | State |
| --- | --- |
| renderer | ready |
Ordinary paragraph after the table.
| later | pipe |
HTML block:
<div>visible html</div>
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");

    assert!(
        rendered.find("Ordinary paragraph").expect("paragraph")
            < rendered.find("| later | pipe |").expect("later pipe row")
    );
    assert!(
        rendered.find("| later | pipe |").expect("later pipe row")
            < rendered.find("HTML block:").expect("HTML label")
    );
    assert_snapshot!("markdown_table_spillover", rendered);
}

#[test]
fn markdown_table_keeps_grid_for_one_fragmented_compact_value() {
    let md = r#"| Key | Date | State |
| --- | --- | --- |
| short | 2025-01-01 | Ready |
| verylongidentifier | 2025-02-02 | Ready |
| final | 2025-03-03 | Done |
"#;
    let lines = plain_lines(&render_markdown_text_with_width(md, Some(40)));

    assert!(lines.iter().any(|line| line.contains('━')));
    assert!(lines.iter().any(|line| line.contains('─')));
    assert!(lines.iter().any(|line| line.contains("verylong")));
}

#[test]
fn markdown_table_uses_records_for_systemic_fragmentation() {
    let md = r#"| Key | Notes |
| --- | --- |
| firstlongid | A readable explanatory sentence for this row. |
| secondlongid | Another readable explanatory sentence for this row. |
| short | A final readable explanatory sentence for this row. |
"#;
    let lines = plain_lines(&render_markdown_text_with_width(md, Some(17)));
    let rendered = lines.join("\n");

    assert!(!rendered.contains('━'));
    assert!(rendered.contains("firstlongid"));
    assert!(rendered.contains("secondlongid"));
    assert!(rendered.contains("explanatory"));
}

#[test]
fn markdown_table_extremely_narrow_records_do_not_lose_content() {
    let md = r#"| One | Two | Three | Four |
| --- | --- | --- | --- |
| alpha | beta | gamma | delta |
"#;
    let text = render_markdown_text_with_width(md, Some(3));
    let rendered = plain_lines(&text).join("\n");
    let compact = rendered
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();

    for expected in [
        "One", "alpha", "Two", "beta", "Three", "gamma", "Four", "delta",
    ] {
        assert!(
            compact.contains(expected),
            "missing {expected:?} from {rendered:?}"
        );
    }
    assert!(!rendered.contains('━'));
    assert!(text.lines.iter().all(|line| line.width() <= 3));
}

#[test]
fn markdown_table_extremely_narrow_wide_records_fit_when_indent_can_shrink() {
    let cjk = render_markdown_text_with_width("| 键 |\n| --- |\n| 值 |\n", Some(3));
    assert!(cjk.lines.iter().all(|line| line.width() <= 3));
    assert_eq!(
        plain_lines(&cjk)
            .join("")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>(),
        "键值"
    );
    assert!(
        plain_lines(&cjk)
            .iter()
            .any(|line| line.starts_with(' ') && line.ends_with('值'))
    );

    let emoji =
        render_markdown_text_with_width("| Icon |\n| --- |\n| 👨‍👩‍👧‍👦 |\n", Some(3));
    assert!(emoji.lines.iter().all(|line| line.width() <= 3));
    assert!(plain_lines(&emoji).join("\n").contains("👨‍👩‍👧‍👦"));

    let ascii = render_markdown_text_with_width("| K |\n| --- |\n| v |\n", Some(3));
    assert!(plain_lines(&ascii).iter().any(|line| line == "  v"));
}

#[test]
fn markdown_table_preserves_header_separator_and_inline_styles() {
    let md = r#"| *Kind* | Content |
| --- | --- |
| rich | **bold** *italic* ~~gone~~ `code` [docs](https://example.com) |
"#;
    let text = render_markdown_text(md);

    let header_span = text.lines[0]
        .spans
        .iter()
        .find(|span| span.content.contains("Kind"))
        .expect("table header span");
    assert!(header_span.style.add_modifier.contains(Modifier::BOLD));
    assert!(header_span.style.add_modifier.contains(Modifier::ITALIC));
    assert!(
        text.lines[1].spans[0]
            .style
            .add_modifier
            .contains(Modifier::DIM)
    );

    let styled_spans = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .collect::<Vec<_>>();
    assert!(styled_spans.iter().any(|span| {
        span.content == "bold" && span.style.add_modifier.contains(Modifier::BOLD)
    }));
    assert!(styled_spans.iter().any(|span| {
        span.content == "italic" && span.style.add_modifier.contains(Modifier::ITALIC)
    }));
    assert!(styled_spans.iter().any(|span| {
        span.content == "gone" && span.style.add_modifier.contains(Modifier::CROSSED_OUT)
    }));
    assert!(
        styled_spans
            .iter()
            .any(|span| span.content == "code" && span.style.fg == Some(Color::Cyan))
    );
    assert!(styled_spans.iter().any(|span| {
        span.content == "https://example.com"
            && span.style.add_modifier.contains(Modifier::UNDERLINED)
    }));

    let narrow = render_markdown_text_with_width(md, Some(6));
    let narrow_header = narrow
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains("Kind"))
        .expect("key-value header span");
    assert!(
        narrow_header
            .style
            .add_modifier
            .contains(Modifier::BOLD | Modifier::ITALIC)
    );
}

#[test]
fn markdown_table_aligns_cjk_emoji_and_combined_emoji_by_display_width() {
    let md = r#"| 名称 | 图标 | 状态 |
| --- | --- | --- |
| 中文 | 🧪 | ready |
| family | 👨‍👩‍👧‍👦 | 完成 |
"#;
    let lines = plain_lines(&render_markdown_text(md));
    let header = lines
        .iter()
        .find(|line| line.contains("名称"))
        .expect("header");
    let first = lines
        .iter()
        .find(|line| line.contains("🧪"))
        .expect("emoji row");
    let second = lines
        .iter()
        .find(|line| line.contains("👨‍👩‍👧‍👦"))
        .unwrap_or_else(|| panic!("combined emoji row: {lines:?}"));

    assert_eq!(
        [
            display_column(header, "图标"),
            display_column(first, "🧪"),
            display_column(second, "👨‍👩‍👧‍👦"),
        ],
        [display_column(header, "图标"); 3]
    );
    assert_eq!(
        [
            display_column(header, "状态"),
            display_column(first, "ready"),
            display_column(second, "完成"),
        ],
        [display_column(header, "状态"); 3]
    );
}

#[test]
fn markdown_table_reserves_blockquote_and_list_prefix_width() {
    let quoted = render_markdown_text_with_width(
        "> | A | B |\n> | --- | --- |\n> | 中文 | 🚀 |\n",
        Some(24),
    );
    assert!(
        quoted
            .lines
            .iter()
            .all(|line| line.width() <= 24 && plain_line(line).starts_with("> "))
    );

    let listed = render_markdown_text_with_width(
        "- Results:\n\n  | A | B |\n  | --- | --- |\n  | 中文 | 🚀 |\n",
        Some(28),
    );
    let listed_lines = plain_lines(&listed);
    let table_lines = listed
        .lines
        .iter()
        .zip(&listed_lines)
        .filter(|(_, line)| line.contains('━') || line.contains("中文"))
        .collect::<Vec<_>>();
    assert!(!table_lines.is_empty());
    assert!(
        table_lines
            .iter()
            .all(|(line, text)| line.width() <= 28 && text.starts_with("  "))
    );
}

#[test]
fn markdown_table_normalizes_row_lengths_and_preserves_pipes() {
    let md = r#"| A | B | C |
| --- | --- | --- |
| one | two |
| x | y | z | ignored |
| a \| b | `c|d` | ok |
"#;
    let rendered = plain_lines(&render_markdown_text(md)).join("\n");

    assert!(rendered.contains("one"));
    assert!(rendered.contains("two"));
    assert!(!rendered.contains("ignored"));
    assert!(rendered.contains("a | b"));
    assert!(rendered.contains("c|d"), "{rendered:?}");
}

#[test]
fn markdown_table_preserves_native_escaped_code_pipe_semantics() {
    let md = r#"| A | B |
| --- | --- |
| `a\|b` | one |
| `a\\|b` | two |
"#;
    let text = render_markdown_text(md);
    let code_spans = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Cyan))
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(code_spans, vec!["a|b", r"a\|b"]);
}

#[test]
fn markdown_table_keeps_lone_backticks_in_separate_cells() {
    let md = r#"| A | B |
| --- | --- |
| ` | ` |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");

    assert!(rendered.contains('━'));
    assert_eq!(rendered.matches('`').count(), 2, "{rendered:?}");
    assert!(
        text.lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .all(|span| !(span.content == "|" && span.style.fg == Some(Color::Cyan)))
    );
}

#[test]
fn markdown_table_prefers_cell_local_code_over_cross_cell_backticks() {
    let md = r#"| A | B | C |
| --- | --- | --- |
| ` | `a|b` | ok |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");

    assert_eq!(rendered.matches('`').count(), 1, "{rendered:?}");
    assert!(rendered.contains("ok"), "{rendered:?}");
    assert!(
        text.lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content == "a|b" && span.style.fg == Some(Color::Cyan))
    );
}

#[test]
fn markdown_table_rejects_code_pipe_masks_that_escape_code_events() {
    let md = r#"| H1 | H2 | H3 |
| --- | --- | --- |
| prefix `x ``p|` longword|value`` suffix | tail |
"#;
    let text = render_markdown_text(md);
    let lines = plain_lines(&text);
    let rendered = lines.join("\n");
    let code = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Cyan))
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>();
    let body = lines
        .iter()
        .find(|line| line.contains("prefix"))
        .expect("table body");
    let first_boundary = &body[body.find("longword").expect("longword") + "longword".len()
        ..body.find("value").expect("value")];
    let second_boundary = &body[body.find("suffix").expect("suffix") + "suffix".len()
        ..body.find("tail").expect("tail")];

    assert_eq!(code, vec!["x ``p|"]);
    assert!(
        !first_boundary.is_empty() && first_boundary.chars().all(char::is_whitespace),
        "{body:?}"
    );
    assert!(
        !second_boundary.is_empty() && second_boundary.chars().all(char::is_whitespace),
        "{body:?}"
    );
    assert!(!rendered.contains('\u{e000}'), "{rendered:?}");
}

#[test]
fn markdown_table_preserves_code_opener_after_escaped_backtick() {
    let md = r#"| A | B |
| --- | --- |
| \``foo|bar` | tail |
"#;
    let text = render_markdown_text(md);
    let lines = plain_lines(&text);
    let rendered = lines.join("\n");
    let code = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Cyan))
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>();
    let body = lines
        .iter()
        .find(|line| line.contains("foo|bar"))
        .expect("table body");
    let boundary = &body[body.find("foo|bar").expect("code") + "foo|bar".len()
        ..body.find("tail").expect("tail")];

    assert_eq!(code, vec!["foo|bar"]);
    assert_eq!(rendered.matches('`').count(), 1);
    assert!(
        !boundary.is_empty() && boundary.chars().all(char::is_whitespace),
        "{body:?}"
    );
}

#[test]
fn markdown_table_preserves_backslash_preceded_code_closers() {
    let md = r#"| `A|B\` | C |
| --- | --- |
| `x|y\` | tail |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");
    let code = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Cyan))
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(code, vec![r"A|B\", r"x|y\"]);
    assert!(rendered.contains('━'), "{rendered:?}");
    assert!(rendered.contains("tail"), "{rendered:?}");
    assert!(!rendered.contains('\u{e000}'), "{rendered:?}");
}

#[test]
fn markdown_table_escaped_literal_backticks_do_not_steal_prior_code_closers() {
    let md = r#"| `Header|Code` | Label\` |
| --- | --- |
| `x|` | longer\` |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");
    let code = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Cyan))
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(code, vec!["Header|Code", "x|"]);
    assert!(rendered.contains("Label`"), "{rendered:?}");
    assert!(rendered.contains("longer`"), "{rendered:?}");
    assert_eq!(rendered.matches('`').count(), 2, "{rendered:?}");
    assert!(rendered.contains('━'), "{rendered:?}");
    assert!(!rendered.contains('\u{e000}'), "{rendered:?}");
}

#[test]
fn markdown_table_masks_code_pipes_before_normalizing_long_and_sparse_rows() {
    let md = r#"| A | B | C |
| --- | --- | --- |
| `a|b` | ok | value | ignored |
| `c|d` | next |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");
    let code = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Cyan))
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(code, vec!["a|b", "c|d"]);
    for expected in ["ok", "value", "next"] {
        assert!(rendered.contains(expected), "missing {expected:?}: {rendered:?}");
    }
    assert!(!rendered.contains("ignored"), "{rendered:?}");
}

#[test]
fn markdown_table_recovers_explicit_header_with_raw_code_pipe() {
    let md = r#"| `A|B` | C |
| --- | --- |
| x | y |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");

    assert!(rendered.contains('━'), "{rendered:?}");
    assert!(rendered.contains("x"), "{rendered:?}");
    assert!(rendered.contains("y"), "{rendered:?}");
    assert!(
        text.lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content == "A|B" && span.style.fg == Some(Color::Cyan))
    );
}

#[test]
fn markdown_table_recovers_blockquoted_explicit_header_with_raw_code_pipe() {
    let md = r#"> | `A|B` | C |
> | --- | --- |
> | x | y |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");

    assert!(rendered.contains('━'), "{rendered:?}");
    assert!(
        text.lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content == "A|B" && span.style.fg == Some(Color::Cyan))
    );
}

#[test]
fn markdown_table_recovers_list_headers_with_raw_code_pipe() {
    let cases = [
        (
            "- ",
            r#"- | `A|B` | C |
  | --- | --- |
  | value | tail |
"#,
        ),
        (
            "1. ",
            r#"1. | `A|B` | C |
   | --- | --- |
   | value | tail |
"#,
        ),
    ];

    for (marker, md) in cases {
        let text = render_markdown_text(md);
        let lines = plain_lines(&text);
        let rendered = lines.join("\n");

        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with(marker))
                .count(),
            1,
            "{marker:?}: {rendered:?}"
        );
        assert!(rendered.contains('━'), "{marker:?}: {rendered:?}");
        assert!(rendered.contains("value"), "{marker:?}: {rendered:?}");
        assert!(rendered.contains("tail"), "{marker:?}: {rendered:?}");
        assert!(
            text.lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content == "A|B" && span.style.fg == Some(Color::Cyan)),
            "{marker:?}: {text:?}"
        );
    }
}

#[test]
fn markdown_table_recognized_light_syntax_body_supports_raw_code_pipe() {
    let md = r#"A | B
--- | ---
`a|b` | ok
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");

    assert!(rendered.contains('━'), "{rendered:?}");
    assert!(rendered.contains("ok"), "{rendered:?}");
    assert!(
        text.lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content == "a|b" && span.style.fg == Some(Color::Cyan))
    );
}

#[test]
fn markdown_table_supports_multiple_embedded_and_long_delimiter_code_spans() {
    let md = r#"| One | Two | Three |
| --- | --- | --- |
| ``a`|b`` | `c|d` and `e|f` | prefix `g | h` suffix |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");
    let code = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Cyan))
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>();

    for expected in ["a`|b", "c|d", "e|f", "g | h"] {
        assert!(code.contains(&expected), "missing {expected:?}: {code:?}");
    }
    for expected in ["and", "prefix", "suffix"] {
        assert!(rendered.contains(expected), "missing {expected:?}: {rendered:?}");
    }
}

#[test]
fn markdown_table_pipe_only_code_requires_standard_escape() {
    let md = r#"| A | B |
| --- | --- |
| ` | ` |
| `\|` | ok |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");

    assert_eq!(rendered.matches('`').count(), 2, "{rendered:?}");
    assert!(rendered.contains("ok"), "{rendered:?}");
    assert!(
        text.lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content == "|" && span.style.fg == Some(Color::Cyan))
    );
}

#[test]
fn markdown_table_code_pipe_mask_never_leaks_sentinel() {
    let md = r#"| `A|B` | C |
| --- | --- |
| `x|y` | ok |
"#;
    let rendered = plain_lines(&render_markdown_text(md)).join("\n");

    assert!(!rendered.contains('\u{e000}'), "{rendered:?}");
    assert!(rendered.contains("A|B"), "{rendered:?}");
    assert!(rendered.contains("x|y"), "{rendered:?}");
}

#[test]
fn markdown_table_body_keeps_unescaped_code_pipe_and_following_cell() {
    let md = r#"| A | B |
| --- | --- |
| `c|d` | ok |
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");
    let code = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content == "c|d")
        .expect("code span");

    assert_eq!(code.style.fg, Some(Color::Cyan));
    assert!(rendered.contains("ok"), "{rendered:?}");
}

#[test]
fn inline_code_pipe_does_not_turn_non_table_into_table() {
    let md = "A | `b|c`\n---|---\n";
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");

    assert!(!rendered.contains('━'), "{rendered:?}");
    assert!(rendered.contains("---|---"), "{rendered:?}");
    assert!(
        text.lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.content == "b|c" && span.style.fg == Some(Color::Cyan))
    );
}

#[test]
fn markdown_table_spillover_ignores_escaped_trailing_pipe_boundary() {
    let md = r#"| A | B |
| --- | --- |
| x | y |
Paragraph ending with \|
"#;
    let text = render_markdown_text(md);

    assert!(
        plain_lines(&text)
            .iter()
            .any(|line| line == "Paragraph ending with |")
    );
}

#[test]
fn markdown_table_spillover_reparses_inline_code_without_synthetic_pipes() {
    let md = r#"| A | B |
| --- | --- |
| x | y |
Plain paragraph
`a|b` | tail
one | two | three
"#;
    let text = render_markdown_text(md);
    let lines = plain_lines(&text);
    let code_line = lines
        .iter()
        .position(|line| line.contains("a|b"))
        .expect("spillover code line");
    let code = text.lines[code_line]
        .spans
        .iter()
        .find(|span| span.content == "a|b")
        .expect("spillover code span");

    assert_eq!(lines[code_line], "a|b | tail");
    assert_eq!(code.style.fg, Some(Color::Cyan));
    assert!(!lines[code_line].contains(r"\|"));
    assert!(lines.iter().any(|line| line == "one | two | three"));
}

#[test]
fn markdown_table_spillover_reparses_contiguous_rows_as_one_fragment() {
    let md = r#"| A | B |
| --- | --- |
| x | y |
This is *one
emphasized phrase*.
"#;
    let text = render_markdown_text(md);
    let lines = plain_lines(&text);
    let italic = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.add_modifier.contains(Modifier::ITALIC))
        .map(|span| span.content.as_ref())
        .collect::<Vec<_>>();

    assert!(lines.iter().any(|line| line == "This is one"));
    assert!(lines.iter().any(|line| line == "emphasized phrase."));
    assert_eq!(italic, vec!["one", "emphasized phrase"]);
}

#[test]
fn markdown_table_spillover_never_leaks_sentinel_after_backticks_repair_across_rows() {
    let cases = [
        (
            "first sentinel",
            r#"| A | B |
| --- | --- |
| x | y |
Plain `
`a|b`
"#
            .to_string(),
            '\u{e000}',
            None,
        ),
        (
            "later sentinel",
            "| A | B |\n| --- | --- |\n| x | y |\nPlain \u{e000} `\n`a|b`\n".to_string(),
            '\u{e001}',
            Some('\u{e000}'),
        ),
    ];

    for (name, md, sentinel, preserved) in cases {
        let text = render_markdown_text(&md);
        let rendered = plain_lines(&text).join("\n");

        assert!(!rendered.contains(sentinel), "{name}: {rendered:?}");
        assert!(rendered.contains("a|b"), "{name}: {rendered:?}");
        assert!(
            text.lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content == " " && span.style.fg == Some(Color::Cyan)),
            "{name}: {text:?}"
        );
        assert!(rendered.ends_with("a|b`"), "{name}: {rendered:?}");
        if let Some(preserved) = preserved {
            assert!(rendered.contains(preserved), "{name}: {rendered:?}");
        }
    }
}

#[test]
fn markdown_table_spillover_resolves_cross_row_reference_link() {
    let md = r#"| A | B |
| --- | --- |
| x | y |
Read [the
docs][ref].

[ref]: https://example.com
"#;
    let text = render_markdown_text(md);
    let rendered = plain_lines(&text).join("\n");
    let urls = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| {
            span.content == "https://example.com"
                && span
                    .style
                    .add_modifier
                    .contains(Modifier::UNDERLINED)
        })
        .count();

    assert!(rendered.contains("Read the"), "{rendered:?}");
    assert!(rendered.contains("docs (https://example.com)."), "{rendered:?}");
    assert_eq!(urls, 1);
}

#[test]
fn markdown_table_spillover_keeps_blockquote_prefix_once() {
    let md = r#"> | A | B |
> | --- | --- |
> | x | y |
> This is *one
> emphasized phrase*.
"#;
    let text = render_markdown_text(md);
    let lines = plain_lines(&text);
    let spillover = lines
        .iter()
        .filter(|line| line.contains("This is") || line.contains("emphasized phrase"))
        .collect::<Vec<_>>();

    assert_eq!(spillover.len(), 2);
    assert!(
        spillover
            .iter()
            .all(|line| line.starts_with("> ") && !line.starts_with("> > "))
    );
    assert!(
        text.lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.add_modifier.contains(Modifier::ITALIC))
            .count()
            >= 2
    );
}

#[test]
fn markdown_table_spillover_keeps_list_continuation_indent() {
    let md = r#"- | A | B |
  | --- | --- |
  | x | y |
  This is *one
  emphasized phrase*.
"#;
    let text = render_markdown_text(md);
    let lines = plain_lines(&text);
    let spillover = lines
        .iter()
        .filter(|line| line.contains("This is") || line.contains("emphasized phrase"))
        .map(String::as_str)
        .collect::<Vec<_>>();

    assert_eq!(
        spillover,
        vec!["  This is one", "  emphasized phrase."]
    );
    assert_eq!(lines.iter().filter(|line| line.starts_with("- ")).count(), 1);
}

#[test]
fn markdown_table_spillover_resolves_reference_definitions_before_and_after_table() {
    let cases = [
        (
            "definition before",
            r#"[docs]: https://example.com "Docs"

| A | B |
| --- | --- |
| x | y |
Plain paragraph
Read [docs].
"#,
        ),
        (
            "definition after",
            r#"| A | B |
| --- | --- |
| x | y |
Plain paragraph
Read [docs].

[docs]: https://example.com "Docs"
"#,
        ),
    ];

    for (name, md) in cases {
        let text = render_markdown_text(md);
        let rendered = plain_lines(&text).join("\n");
        assert!(
            rendered.contains("Read docs (https://example.com)."),
            "{name}: {rendered:?}"
        );
        assert!(
            text.lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| {
                    span.content == "https://example.com"
                        && span
                            .style
                            .add_modifier
                            .contains(Modifier::UNDERLINED)
                }),
            "{name}: {text:?}"
        );
    }
}

#[test]
fn markdown_table_spillover_keeps_unresolved_reference_literal() {
    let md = r#"| A | B |
| --- | --- |
| x | y |
Plain paragraph
Read [missing].
"#;
    let rendered = plain_lines(&render_markdown_text(md)).join("\n");

    assert!(rendered.contains("Read [missing]."), "{rendered:?}");
}

#[test]
fn markdown_table_spillover_preserves_source_boundaries() {
    let md = r#"| A | B |
| --- | --- |
| x | y |
Plain paragraph
| later | pipe |
"#;
    let lines = plain_lines(&render_markdown_text(md));

    assert!(lines.iter().any(|line| line == "Plain paragraph"));
    assert!(lines.iter().any(|line| line == "| later | pipe |"));
    assert!(!lines.iter().any(|line| line == "| Plain paragraph |"));
}

#[test]
fn markdown_table_pipe_fallback_does_not_reescape_rendered_code() {
    let md = r#"| `a|b` | C |
| --- | --- |
"#;
    let text = render_markdown_text_with_width(md, Some(1));
    let rendered = plain_lines(&text).join("\n");
    let code = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .filter(|span| span.style.fg == Some(Color::Cyan))
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(code, "a|b");
    assert!(!rendered.contains(r"\|"), "{rendered:?}");
}

#[test]
fn markdown_table_pipe_fallback_keeps_zwj_header_whole_and_styled() {
    let family = "👨‍👩‍👧‍👦";
    let md = format!("| {family} | C |\n| --- | --- |\n");
    let text = render_markdown_text_with_width(&md, Some(1));
    let header = text
        .lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content == family)
        .expect("whole family emoji header span");

    assert!(header.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn markdown_table_interior_pipe_keeps_pulldown_backslash_semantics() {
    for backslash_count in 0..=5 {
        let backslashes = "\\".repeat(backslash_count);
        let md = format!(
            "| A | B |\n| --- | --- |\n| left{backslashes}|right | tail |\n"
        );
        let rendered = plain_lines(&render_markdown_text(&md)).join("\n");

        assert!(rendered.contains("right"), "{backslash_count}: {rendered:?}");
        assert_eq!(
            rendered.contains("tail"),
            backslash_count > 0,
            "{backslash_count}: {rendered:?}"
        );
    }
}

#[test]
fn markdown_table_code_pipe_backslashes_follow_native_semantics() {
    for backslash_count in 0..=5 {
        let backslashes = "\\".repeat(backslash_count);
        let md = format!(
            "| A | B |\n| --- | --- |\n| `a{backslashes}|b` | row-{backslash_count} |\n"
        );
        let text = render_markdown_text(&md);
        let rendered = plain_lines(&text).join("\n");
        let code = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.fg == Some(Color::Cyan))
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let expected = format!("a{}|b", "\\".repeat(backslash_count.saturating_sub(1)));

        assert_eq!(code, expected, "{backslash_count}: {text:?}");
        assert!(
            rendered.contains(&format!("row-{backslash_count}")),
            "{backslash_count}: {rendered:?}"
        );
        assert!(!rendered.contains('\u{e000}'), "{backslash_count}: {rendered:?}");
    }
}

#[test]
fn markdown_table_inside_code_fence_stays_code() {
    let md = "```markdown\n| A | B |\n| --- | --- |\n| 1 | 2 |\n```\n";
    let rendered = plain_lines(&render_markdown_text(md)).join("\n");

    assert!(rendered.contains("| A | B |"));
    assert!(rendered.contains("| --- | --- |"));
    assert!(!rendered.contains('━'));
}

#[test]
fn inline_code_pipe_outside_table_is_unchanged() {
    let text = render_markdown_text("Use `a|b` here.");
    assert_eq!(plain_lines(&text), vec!["Use a|b here."]);
    let code = text.lines[0]
        .spans
        .iter()
        .find(|span| span.content == "a|b")
        .expect("inline code span");
    assert_eq!(code.style.fg, Some(Color::Cyan));
}
