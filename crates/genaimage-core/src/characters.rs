//! First-class characters: versioned specs, references and output decisions.
//!
//! Everything here is append-only. A spec version is never edited, a reference
//! is only ever soft-removed, and a decision is undone by a later `cleared`
//! decision. Every write is a single SQLite transaction, so an error means that
//! nothing was written.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{HarnessError, Result};

mod service;
#[cfg(test)]
mod tests;

pub use service::{
    CharacterListQuery, CharacterPage, CharacterRunQuery, CharacterService, CreatedCharacter,
    CurrentStateQuery, DecisionListQuery, DecisionOutputPage, DecisionPage, DecisionWithOutput,
    NewCharacter, NewDecision, NewReference, NewSpecVersion, ReferenceListQuery, ReferencePage,
    SpecVersionListQuery, SpecVersionPage,
};

pub const NAME_MAX_BYTES: usize = 200;
pub const SUMMARY_MAX_BYTES: usize = 2_000;
pub const STYLE_NOTES_MAX_BYTES: usize = 4_000;
pub const NOTE_MAX_BYTES: usize = 1_000;
pub const MAX_ELEMENTS: usize = 64;
pub const ELEMENT_ID_MAX_BYTES: usize = 48;
pub const ELEMENT_DESCRIPTION_MAX_BYTES: usize = 1_000;
pub const MAX_PALETTE_ENTRIES: usize = 32;
pub const PALETTE_NAME_MAX_BYTES: usize = 64;
pub const PALETTE_ROLE_MAX_BYTES: usize = 64;
pub const MAX_ACTIVE_REFERENCES: usize = 32;
pub const OUTPUT_ID_MAX_BYTES: usize = 128;
/// Upper bound on the serialised spec, whatever the individual limits allow.
pub const SPEC_MAX_BYTES: usize = 64 * 1024;
pub const DEFAULT_PAGE_LIMIT: usize = 50;
pub const MAX_PAGE_LIMIT: usize = 200;
pub const IDEMPOTENCY_KEY_MAX_BYTES: usize = 128;

/// Names one write so that a retry replays it instead of repeating it.
///
/// The same key with the same request returns the record written the first
/// time; the same key with another request is `IdempotencyConflict`. A key
/// belongs to one operation on one character store, whichever method used it
/// first. The assistant uses its confirmation ID; the server uses the
/// `Idempotency-Key` header, which allows the same characters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// 1 to 128 visible ASCII characters, without commas.
    pub fn parse(key: &str) -> Result<Self> {
        let valid = !key.is_empty()
            && key.len() <= IDEMPOTENCY_KEY_MAX_BYTES
            && key.bytes().all(|b| b.is_ascii_graphic() && b != b',');
        if valid {
            Ok(Self(key.to_owned()))
        } else {
            Err(invalid(format!(
                "An idempotency key must be 1 to {IDEMPOTENCY_KEY_MAX_BYTES} visible ASCII characters, without commas."
            )))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The spec version of a character that a run was submitted against.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterLink {
    pub id: String,
    pub spec_version: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Character {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// `None` until the first spec version is created.
    pub latest_spec_version: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ElementCategory {
    Silhouette,
    Proportions,
    Anatomy,
    Clothing,
    Equipment,
    Materials,
    Colour,
    Other,
}

impl ElementCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Silhouette => "silhouette",
            Self::Proportions => "proportions",
            Self::Anatomy => "anatomy",
            Self::Clothing => "clothing",
            Self::Equipment => "equipment",
            Self::Materials => "materials",
            Self::Colour => "colour",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Requirement {
    Mandatory,
    Flexible,
    Unspecified,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Front,
    Side,
    Back,
    ThreeQuarter,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SpecElement {
    /// A stable slug, unique within one spec: lowercase letters, digits, `-` and `_`.
    pub id: String,
    pub category: ElementCategory,
    pub description: String,
    pub requirement: Requirement,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteEntry {
    pub name: String,
    /// `#rrggbb`, kept exactly as given.
    #[serde(default)]
    pub hex: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CharacterSpec {
    pub summary: String,
    #[serde(default)]
    pub elements: Vec<SpecElement>,
    #[serde(default)]
    pub palette: Vec<PaletteEntry>,
    #[serde(default)]
    pub views: Vec<View>,
    #[serde(default)]
    pub style_notes: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CharacterSpecVersion {
    pub character_id: String,
    /// 1, 2, 3 and so on, without gaps.
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub based_on: Option<u32>,
    pub spec: CharacterSpec,
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRole {
    Sketch,
    Reference,
    Mask,
}

impl ReferenceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sketch => "sketch",
            Self::Reference => "reference",
            Self::Mask => "mask",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CharacterReference {
    pub id: String,
    pub character_id: String,
    pub upload_id: String,
    pub role: ReferenceRole,
    pub note: Option<String>,
    pub added_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approved,
    Rejected,
    /// Undoes the previous decision; the output then has no current state.
    Cleared,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Cleared => "cleared",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OutputDecision {
    pub id: String,
    pub character_id: String,
    pub output_id: String,
    pub spec_version: u32,
    pub decision: Decision,
    pub note: Option<String>,
    pub decided_at: DateTime<Utc>,
}

fn invalid(message: String) -> HarnessError {
    HarnessError::InvalidCharacterRequest(message)
}

/// Unicode format characters (category Cf) and the line and paragraph
/// separators. Written as explicit ranges to avoid a dependency.
fn is_invisible_formatting(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// Bidirectional overrides, embeddings and isolates can reorder text on screen.
fn is_bidi_control(character: char) -> bool {
    matches!(character, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn check_text(
    field: &str,
    value: &str,
    max_bytes: usize,
    required: bool,
    multiline: bool,
) -> Result<()> {
    if required && value.trim().is_empty() {
        return Err(invalid(format!("{field} must not be empty.")));
    }
    if value.len() > max_bytes {
        return Err(invalid(format!(
            "{field} must be at most {max_bytes} bytes; it is {} bytes.",
            value.len()
        )));
    }
    if multiline {
        let allowed = |character: char| {
            (!character.is_control() || matches!(character, '\n' | '\r' | '\t'))
                && !is_bidi_control(character)
        };
        if !value.chars().all(allowed) {
            return Err(invalid(format!(
                "{field} must not contain control characters other than line breaks and tabs, or bidirectional overrides."
            )));
        }
    } else if !value
        .chars()
        .all(|character| !character.is_control() && !is_invisible_formatting(character))
    {
        return Err(invalid(format!(
            "{field} must be a single line without control, invisible formatting or line separator characters."
        )));
    }
    Ok(())
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    check_text("name", name, NAME_MAX_BYTES, true, false)
}

pub(crate) fn validate_note(field: &str, note: Option<&str>) -> Result<()> {
    note.map_or(Ok(()), |note| {
        check_text(field, note, NOTE_MAX_BYTES, false, true)
    })
}

/// An output ID has exactly one spelling, `{run_id}:{row}`, because it is a
/// key: `run:+1` or `run:01` would otherwise split one output's history.
pub(crate) fn validate_output_id(output_id: &str) -> Result<()> {
    check_text("output_id", output_id, OUTPUT_ID_MAX_BYTES, true, false)?;
    if crate::persistence::parse_output_id(output_id).is_none() {
        return Err(invalid(
            "output_id must be an output ID exactly as the output lists report it: {run_id}:{row}, with the row as plain decimal digits.".into(),
        ));
    }
    Ok(())
}

fn is_slug(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn is_hex_colour(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

impl CharacterSpec {
    /// Rejects the whole spec on the first problem; nothing is clamped, trimmed
    /// or dropped.
    pub fn validate(&self) -> Result<()> {
        check_text("spec.summary", &self.summary, SUMMARY_MAX_BYTES, true, true)?;
        if let Some(notes) = &self.style_notes {
            check_text(
                "spec.style_notes",
                notes,
                STYLE_NOTES_MAX_BYTES,
                false,
                true,
            )?;
        }
        if self.elements.len() > MAX_ELEMENTS {
            return Err(invalid(format!(
                "spec.elements may hold at most {MAX_ELEMENTS} elements; it holds {}.",
                self.elements.len()
            )));
        }
        let mut seen = BTreeSet::new();
        for (index, element) in self.elements.iter().enumerate() {
            let field = format!("spec.elements[{index}]");
            check_text(
                &format!("{field}.id"),
                &element.id,
                ELEMENT_ID_MAX_BYTES,
                true,
                false,
            )?;
            if !is_slug(&element.id) {
                return Err(invalid(format!(
                    "{field}.id '{}' is not a slug: use lowercase letters, digits, '-' and '_', starting with a letter or digit.",
                    element.id
                )));
            }
            if !seen.insert(element.id.as_str()) {
                return Err(invalid(format!(
                    "{field}.id '{}' is used by an earlier element; element ids must be unique within a spec.",
                    element.id
                )));
            }
            check_text(
                &format!("{field}.description"),
                &element.description,
                ELEMENT_DESCRIPTION_MAX_BYTES,
                true,
                true,
            )?;
        }
        if self.palette.len() > MAX_PALETTE_ENTRIES {
            return Err(invalid(format!(
                "spec.palette may hold at most {MAX_PALETTE_ENTRIES} entries; it holds {}.",
                self.palette.len()
            )));
        }
        for (index, entry) in self.palette.iter().enumerate() {
            let field = format!("spec.palette[{index}]");
            check_text(
                &format!("{field}.name"),
                &entry.name,
                PALETTE_NAME_MAX_BYTES,
                true,
                false,
            )?;
            if let Some(hex) = &entry.hex {
                if !is_hex_colour(hex) {
                    return Err(invalid(format!(
                        "{field}.hex '{}' is not a colour: use #rrggbb with six hexadecimal digits.",
                        hex.chars().take(32).collect::<String>()
                    )));
                }
            }
            if let Some(role) = &entry.role {
                check_text(
                    &format!("{field}.role"),
                    role,
                    PALETTE_ROLE_MAX_BYTES,
                    true,
                    false,
                )?;
            }
        }
        let distinct: BTreeSet<_> = self.views.iter().collect();
        if distinct.len() != self.views.len() {
            return Err(invalid("spec.views must not repeat a view.".into()));
        }
        let size = serde_json::to_vec(self)?.len();
        if size > SPEC_MAX_BYTES {
            return Err(invalid(format!(
                "The spec serialises to {size} bytes; the limit is {SPEC_MAX_BYTES} bytes."
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn spec() -> CharacterSpec {
        CharacterSpec {
            summary: "A lighthouse keeper".into(),
            elements: vec![SpecElement {
                id: "hat".into(),
                category: ElementCategory::Clothing,
                description: "Yellow sou'wester".into(),
                requirement: Requirement::Mandatory,
            }],
            palette: vec![PaletteEntry {
                name: "Oilskin".into(),
                hex: Some("#FFcc00".into()),
                role: Some("primary".into()),
            }],
            views: vec![View::Front, View::ThreeQuarter],
            style_notes: None,
        }
    }

    fn rejection(spec: &CharacterSpec) -> String {
        match spec.validate() {
            Err(HarnessError::InvalidCharacterRequest(message)) => message,
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn accepts_a_well_formed_spec() {
        spec().validate().unwrap();
    }

    #[test]
    fn serde_spellings_are_part_of_the_contract() {
        let json = serde_json::to_value(spec()).unwrap();
        assert_eq!(json["views"], serde_json::json!(["front", "three_quarter"]));
        assert_eq!(json["elements"][0]["category"], "clothing");
        assert_eq!(json["elements"][0]["requirement"], "mandatory");
        assert!(json["style_notes"].is_null());
        assert_eq!(
            serde_json::to_value(ElementCategory::Colour).unwrap(),
            "colour"
        );
    }

    #[test]
    fn unknown_fields_and_values_are_rejected_not_ignored() {
        let unknown_field = serde_json::json!({"summary": "x", "colour_notes": "y"});
        assert!(serde_json::from_value::<CharacterSpec>(unknown_field).is_err());
        let unknown_view = serde_json::json!({"summary": "x", "views": ["top"]});
        assert!(serde_json::from_value::<CharacterSpec>(unknown_view).is_err());
        let minimal: CharacterSpec = serde_json::from_str(r#"{"summary": "x"}"#).unwrap();
        assert!(minimal.elements.is_empty() && minimal.views.is_empty());
    }

    #[test]
    fn rejects_duplicate_element_ids_and_names_the_clash() {
        let mut spec = spec();
        spec.elements.push(spec.elements[0].clone());
        let message = rejection(&spec);
        assert!(message.contains("spec.elements[1].id 'hat'"), "{message}");
    }

    #[test]
    fn rejects_bad_slugs() {
        for bad in ["Hat", "-hat", "hat cap", "hät", ""] {
            let mut spec = spec();
            spec.elements[0].id = bad.into();
            rejection(&spec);
        }
    }

    #[test]
    fn rejects_bad_palette_hex() {
        for bad in ["ffcc00", "#fc0", "#ffcc0", "#ffcc000", "#gggggg", "#ffcc0é"] {
            let mut spec = spec();
            spec.palette[0].hex = Some(bad.into());
            let message = rejection(&spec);
            assert!(message.contains("spec.palette[0].hex"), "{message}");
        }
    }

    #[test]
    fn enforces_every_count_and_length_bound() {
        let mut too_many_elements = spec();
        too_many_elements.elements = (0..=MAX_ELEMENTS)
            .map(|index| SpecElement {
                id: format!("e{index}"),
                ..spec().elements[0].clone()
            })
            .collect();
        rejection(&too_many_elements);

        let mut too_many_colours = spec();
        too_many_colours.palette = vec![spec().palette[0].clone(); MAX_PALETTE_ENTRIES + 1];
        rejection(&too_many_colours);

        for edit in [
            (|spec: &mut CharacterSpec| spec.summary = "s".repeat(SUMMARY_MAX_BYTES + 1))
                as fn(&mut CharacterSpec),
            |spec| spec.summary = "  ".into(),
            |spec| spec.style_notes = Some("n".repeat(STYLE_NOTES_MAX_BYTES + 1)),
            |spec| spec.elements[0].description = "d".repeat(ELEMENT_DESCRIPTION_MAX_BYTES + 1),
            |spec| spec.elements[0].description = String::new(),
            |spec| spec.palette[0].name = "n".repeat(PALETTE_NAME_MAX_BYTES + 1),
            |spec| spec.palette[0].role = Some("r".repeat(PALETTE_ROLE_MAX_BYTES + 1)),
            |spec| spec.palette[0].name = "two\nlines".into(),
            |spec| spec.summary = "nul\0".into(),
            |spec| spec.views = vec![View::Back, View::Back],
        ] {
            let mut spec = spec();
            edit(&mut spec);
            rejection(&spec);
        }
    }

    #[test]
    fn lengths_are_counted_in_bytes() {
        let mut spec = spec();
        spec.summary = "é".repeat(SUMMARY_MAX_BYTES / 2 + 1);
        assert!(spec.summary.chars().count() <= SUMMARY_MAX_BYTES);
        let message = rejection(&spec);
        assert!(message.contains("bytes"), "{message}");
    }

    #[test]
    fn the_whole_spec_is_bounded_even_when_every_part_is_within_its_limit() {
        let mut spec = spec();
        spec.summary = "s".repeat(SUMMARY_MAX_BYTES);
        spec.style_notes = Some("n".repeat(STYLE_NOTES_MAX_BYTES));
        spec.elements = (0..MAX_ELEMENTS)
            .map(|index| SpecElement {
                id: format!("e{index}"),
                description: "é".repeat(ELEMENT_DESCRIPTION_MAX_BYTES / 2),
                ..spec.elements[0].clone()
            })
            .collect();
        let message = rejection(&spec);
        assert!(message.contains("serialises to"), "{message}");
        spec.elements.truncate(MAX_ELEMENTS / 2);
        spec.validate().unwrap();
    }
}

#[cfg(test)]
mod text_tests {
    use super::*;

    fn invalid_text(result: Result<()>) {
        assert!(
            matches!(result, Err(HarnessError::InvalidCharacterRequest(_))),
            "{result:?}"
        );
    }

    #[test]
    fn single_line_fields_reject_invisible_formatting_and_line_separators() {
        for character in [
            '\u{00AD}',
            '\u{200B}',
            '\u{200C}',
            '\u{200D}',
            '\u{200E}',
            '\u{200F}',
            '\u{2028}',
            '\u{2029}',
            '\u{202A}',
            '\u{202E}',
            '\u{2060}',
            '\u{2066}',
            '\u{2069}',
            '\u{FEFF}',
            '\u{061C}',
            '\u{E0041}',
            '\n',
            '\u{85}',
        ] {
            let value = format!("Keeper{character}x");
            invalid_text(validate_name(&value));
            invalid_text(check_text("field", &value, 100, false, false));
            invalid_text(validate_output_id(&format!("run{character}:1")));
        }
        for (start, end) in [('\u{202A}', '\u{202E}'), ('\u{2066}', '\u{2069}')] {
            for character in start..=end {
                invalid_text(validate_name(&format!("a{character}b")));
            }
        }
        validate_name("Zoë Łukasz 日本語 \u{1F642} د").unwrap();
    }

    #[test]
    fn free_text_rejects_bidirectional_controls_but_keeps_layout() {
        for (start, end) in [('\u{202A}', '\u{202E}'), ('\u{2066}', '\u{2069}')] {
            for character in start..=end {
                let value = format!("harmless{character}text");
                invalid_text(check_text("field", &value, 100, false, true));
                invalid_text(validate_note("note", Some(&value)));
                let spec = CharacterSpec {
                    summary: value,
                    elements: vec![],
                    palette: vec![],
                    views: vec![],
                    style_notes: None,
                };
                invalid_text(spec.validate());
            }
        }
        validate_note("note", Some("line one\r\nline two\tindented")).unwrap();
        invalid_text(validate_note("note", Some("bell\u{7}")));
        // Zero-width characters are layout-neutral in prose, unlike bidi controls.
        validate_note("note", Some("zero\u{200B}width")).unwrap();
    }
}
