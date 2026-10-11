//! The `{placeholder}` templates of `hud.format` and `border.title`.
//!
//! `{{` and `}}` stand for literal braces.

use std::fmt;

/// A value a template can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Note,
    Octave,
    Hz,
    Mode,
    Shape,
    Theme,
    Fps,
    Particles,
    Source,
    Energy,
}

impl Field {
    pub const ALL: [Field; 10] = [
        Field::Note,
        Field::Octave,
        Field::Hz,
        Field::Mode,
        Field::Shape,
        Field::Theme,
        Field::Fps,
        Field::Particles,
        Field::Source,
        Field::Energy,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Octave => "octave",
            Self::Hz => "hz",
            Self::Mode => "mode",
            Self::Shape => "shape",
            Self::Theme => "theme",
            Self::Fps => "fps",
            Self::Particles => "particles",
            Self::Source => "source",
            Self::Energy => "energy",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|field| field.name() == name)
    }
}

/// A piece of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    Literal(String),
    Field(Field),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("unknown placeholder {{{0}}}; allowed: {allowed}", allowed = allowed())]
    Unknown(String),
    #[error("`{{` without a closing `}}`; write `{{{{` for a literal brace")]
    Unclosed,
    #[error("`}}` without an opening `{{`; write `}}}}` for a literal brace")]
    Unopened,
}

fn allowed() -> String {
    Field::ALL
        .iter()
        .map(|field| format!("{{{}}}", field.name()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Split a template into literal text and placeholders.
pub fn parse(template: &str) -> Result<Vec<Segment>, TemplateError> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut characters = template.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' if characters.peek() == Some(&'{') => {
                characters.next();
                literal.push('{');
            }
            '}' if characters.peek() == Some(&'}') => {
                characters.next();
                literal.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match characters.next() {
                        Some('}') => break,
                        Some(character) => name.push(character),
                        None => return Err(TemplateError::Unclosed),
                    }
                }
                let field = Field::from_name(&name).ok_or(TemplateError::Unknown(name))?;
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(Segment::Field(field));
            }
            '}' => return Err(TemplateError::Unopened),
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(segments)
}

impl fmt::Display for Field {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{{{}}}", self.name())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn lit(text: &str) -> Segment {
        Segment::Literal(text.to_owned())
    }

    #[test]
    fn the_default_format_splits_into_text_and_placeholders() {
        assert_eq!(
            parse("{note}{octave}  {mode}  {fps} fps").unwrap(),
            [
                Segment::Field(Field::Note),
                Segment::Field(Field::Octave),
                lit("  "),
                Segment::Field(Field::Mode),
                lit("  "),
                Segment::Field(Field::Fps),
                lit(" fps"),
            ]
        );
        assert_eq!(parse("").unwrap(), []);
        assert_eq!(parse(" plain ").unwrap(), [lit(" plain ")]);
    }

    #[test]
    fn every_documented_placeholder_parses() {
        for field in Field::ALL {
            assert_eq!(
                parse(&format!("<{}>", field)).unwrap(),
                [lit("<"), Segment::Field(field), lit(">")]
            );
        }
        assert_eq!(Field::ALL.len(), 10);
    }

    #[test]
    fn doubled_braces_are_literal_and_mistakes_are_explained() {
        assert_eq!(parse("{{note}}").unwrap(), [lit("{note}")]);
        assert_eq!(parse("a}}b{{c").unwrap(), [lit("a}b{c")]);
        let unknown = parse("{pitch}").unwrap_err().to_string();
        assert!(
            unknown.contains("{pitch}")
                && unknown.contains("{note}")
                && unknown.contains("{energy}"),
            "{unknown}"
        );
        assert!(
            parse("{}")
                .unwrap_err()
                .to_string()
                .contains("unknown placeholder {}")
        );
        assert_eq!(parse("{note").unwrap_err(), TemplateError::Unclosed);
        assert_eq!(parse("fps}").unwrap_err(), TemplateError::Unopened);
        assert!(TemplateError::Unclosed.to_string().contains("{{"));
        // Placeholder names are case-sensitive and take no spaces.
        assert!(parse("{Note}").is_err() && parse("{ note }").is_err());
    }
}
