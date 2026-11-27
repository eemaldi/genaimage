use serde_json::{json, Map, Value};
use tempfile::TempDir;

use super::*;

fn definition(
    id: &str,
    text: &str,
    variables: &[(&str, VariableDefinition)],
) -> TemplateDefinition {
    TemplateDefinition {
        id: id.into(),
        title: "A title".into(),
        description: None,
        text: text.into(),
        variables: variables
            .iter()
            .map(|(name, variable)| ((*name).into(), variable.clone()))
            .collect(),
    }
}

fn required() -> VariableDefinition {
    VariableDefinition {
        required: true,
        ..Default::default()
    }
}

fn optional(default: &str) -> VariableDefinition {
    VariableDefinition {
        default: Some(default.into()),
        ..Default::default()
    }
}

fn problems(text: &str, variables: &[(&str, VariableDefinition)]) -> Vec<(String, String)> {
    definition("t", text, variables)
        .problems(None)
        .into_iter()
        .map(|item| (item.key, item.reason))
        .collect()
}

fn template(text: &str, variables: &[(&str, VariableDefinition)]) -> Template {
    let bytes = definition("t", text, variables).to_file_bytes().unwrap();
    load(
        "user:t".into(),
        TemplateScope::User,
        None,
        Some("t"),
        &bytes,
    )
    .unwrap()
}

fn vars(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn render(template: &Template, value: Value) -> Result<String> {
    template.render(&vars(value))
}

fn render_problems(template: &Template, value: Value) -> Vec<(String, String)> {
    match render(template, value) {
        Err(TemplateError::RenderInvalid(items)) => items
            .into_iter()
            .map(|item| (item.key, item.reason))
            .collect(),
        other => panic!("expected a render problem, got {other:?}"),
    }
}

fn pair(key: &str, reason: &str) -> (String, String) {
    (key.into(), reason.into())
}

#[test]
fn an_undeclared_placeholder_is_invalid() {
    assert_eq!(
        problems("a {{who}}", &[]),
        [pair(
            "text",
            "uses the undeclared placeholder '{{who}}'; declare it under [variables.who]"
        )]
    );
}

#[test]
fn an_unused_variable_is_invalid() {
    assert_eq!(
        problems("plain", &[("who", required())]),
        [pair(
            "variables.who",
            "is declared but not used in the text"
        )]
    );
}

#[test]
fn a_required_variable_with_a_default_is_invalid() {
    let both = VariableDefinition {
        required: true,
        default: Some("x".into()),
        ..Default::default()
    };
    assert_eq!(
        problems("{{who}}", &[("who", both)]),
        [pair(
            "variables.who",
            "is required and also has a default; remove one"
        )]
    );
}

#[test]
fn an_optional_variable_needs_a_default() {
    assert_eq!(
        problems("{{who}}", &[("who", VariableDefinition::default())]),
        [pair(
            "variables.who",
            "is optional without a default; set required = true or give a default (an empty default is allowed)"
        )]
    );
    assert_eq!(problems("{{who}}", &[("who", optional(""))]), []);
}

#[test]
fn malformed_braces_are_invalid() {
    for (text, at) in [
        ("{{ who }}", 0),
        ("{{Who}}", 0),
        ("{{who", 0),
        ("{{who}", 0),
        ("{{}}", 0),
        ("{{1a}}", 0),
        ("ab {{{who}}", 3),
        ("{{{{{{{who}}", 4),
    ] {
        let found = problems(text, &[("who", required())]);
        assert!(
            found.iter().any(|(key, reason)| key == "text"
                && reason.starts_with(&format!("malformed '{{{{' at byte {at}:"))),
            "{text:?} gave {found:?}"
        );
    }
}

#[test]
fn a_name_longer_than_64_characters_is_malformed() {
    let long = "a".repeat(65);
    assert!(problems(&format!("{{{{{long}}}}}"), &[])
        .iter()
        .any(|(_, reason)| reason.starts_with("malformed")));
    let fits = "a".repeat(64);
    assert_eq!(
        problems(&format!("{{{{{fits}}}}}"), &[(fits.as_str(), required())]),
        []
    );
}

#[test]
fn four_open_braces_are_one_literal_pair_and_nothing_else_is_special() {
    let t = template("{{{{who}} {{who}} { } }} {{{{{{{{", &[("who", required())]);
    assert_eq!(
        render(&t, json!({"who": "Ada"})).unwrap(),
        "{{who}} Ada { } }} {{{{"
    );
}

#[test]
fn six_braces_are_a_literal_pair_followed_by_a_placeholder() {
    let t = template("{{{{{{who}}", &[("who", required())]);
    assert_eq!(render(&t, json!({"who": "Ada"})).unwrap(), "{{Ada");
}

#[test]
fn values_are_not_scanned_again() {
    let t = template("{{a}}|{{b}}", &[("a", required()), ("b", required())]);
    assert_eq!(
        render(&t, json!({"a": "{{b}}", "b": "{{{{"})).unwrap(),
        "{{b}}|{{{{"
    );
}

#[test]
fn bounds_are_enforced() {
    assert_eq!(
        problems(&"x".repeat(MAX_TEXT_BYTES + 1), &[]),
        [pair("text", "must be 1 to 16384 bytes")]
    );
    assert_eq!(problems(&"x".repeat(MAX_TEXT_BYTES), &[]), []);
    assert_eq!(
        problems("", &[]),
        [pair("text", "must be 1 to 16384 bytes")]
    );
    let many: Vec<(String, VariableDefinition)> =
        (0..33).map(|n| (format!("v{n}"), required())).collect();
    let many: Vec<(&str, VariableDefinition)> =
        many.iter().map(|(n, v)| (n.as_str(), v.clone())).collect();
    let text: String = many.iter().map(|(n, _)| format!("{{{{{n}}}}}")).collect();
    assert_eq!(
        problems(&text, &many),
        [pair("variables", "at most 32 variables, found 33")]
    );
    assert_eq!(
        problems(
            "{{v}}",
            &[("v", optional(&"x".repeat(MAX_VALUE_BYTES + 1)))]
        ),
        [pair("variables.v", "default is longer than 4096 bytes")]
    );
}

#[test]
fn id_and_variable_names_are_checked() {
    let mut bad = definition("Bad", "x", &[]);
    assert_eq!(bad.problems(None)[0].key, "id");
    bad.id = "../x".into();
    assert_eq!(bad.problems(None)[0].key, "id");
    assert_eq!(
        definition("a", "x", &[]).problems(Some("b")),
        [issue("id", "must equal the file name 'b'")]
    );
    assert_eq!(problems("x", &[("Bad", required())])[0].0, "variables.Bad");
}

#[test]
fn unknown_toml_fields_are_rejected_and_the_file_parses_strictly() {
    let source =
        "id = 't'\ntitle = 'T'\ntext = 'a {{x}}'\nextra = 1\n[variables.x]\nrequired = true\n";
    let error = load(
        "user:t".into(),
        TemplateScope::User,
        None,
        Some("t"),
        source.as_bytes(),
    )
    .unwrap_err();
    assert!(
        error[0].reason.contains("unknown field `extra`"),
        "{error:?}"
    );
    let source =
        "id = 't'\ntitle = 'T'\ntext = 'a {{x}}'\n[variables.x]\nrequired = true\nnote = 'n'\n";
    let error = load(
        "user:t".into(),
        TemplateScope::User,
        None,
        Some("t"),
        source.as_bytes(),
    )
    .unwrap_err();
    assert!(
        error[0].reason.contains("unknown field `note`"),
        "{error:?}"
    );
}

#[test]
fn the_documented_file_format_parses() {
    let source = r#"
id = "portrait-studio"
title = "Studio portrait"
description = "optional"
text = "A {{subject}} photographed in a studio, {{lighting}} lighting"
[variables.subject]
description = "Who or what"
required = true
[variables.lighting]
description = "optional"
default = "soft"
"#;
    let parsed = load(
        "user:portrait-studio".into(),
        TemplateScope::User,
        None,
        Some("portrait-studio"),
        source.as_bytes(),
    )
    .unwrap();
    assert_eq!(parsed.variables.len(), 2);
    assert_eq!(parsed.variables[0].name, "lighting");
    assert_eq!(parsed.variables[0].default.as_deref(), Some("soft"));
    assert_eq!(parsed.variables[1].name, "subject");
    assert!(parsed.variables[1].required);
    assert_eq!(parsed.sha256, sha256_hex(source.as_bytes()));
}

#[test]
fn defaults_apply_only_to_absent_variables_and_an_empty_string_is_a_value() {
    let t = template(
        "[{{subject}}] [{{lighting}}]",
        &[("subject", required()), ("lighting", optional("soft"))],
    );
    assert_eq!(
        render(&t, json!({"subject": "cat"})).unwrap(),
        "[cat] [soft]"
    );
    assert_eq!(
        render(&t, json!({"subject": "cat", "lighting": ""})).unwrap(),
        "[cat] []"
    );
    assert_eq!(
        render(&t, json!({"subject": " cat\n", "lighting": "hard"})).unwrap(),
        "[ cat\n] [hard]"
    );
}

#[test]
fn render_rejects_missing_unknown_and_non_string_values_with_every_problem() {
    let t = template(
        "{{a}} {{b}} {{c}}",
        &[("a", required()), ("b", required()), ("c", optional("d"))],
    );
    assert_eq!(
        render_problems(&t, json!({"c": 3, "z": "x", "y": 1})),
        [
            pair("a", "is required and was not supplied"),
            pair("b", "is required and was not supplied"),
            pair("c", "must be a string"),
            pair("y", "is not a variable of this template"),
            pair("z", "is not a variable of this template"),
        ]
    );
    assert_eq!(
        render_problems(&t, json!({"a": null, "b": "x"})),
        [pair("a", "must be a string")]
    );
}

#[test]
fn render_bounds_values_output_and_problem_count() {
    let t = template("{{a}}", &[("a", required())]);
    assert_eq!(
        render_problems(&t, json!({"a": "x".repeat(MAX_VALUE_BYTES + 1)})),
        [pair("a", "is longer than 4096 bytes")]
    );
    assert_eq!(
        render(&t, json!({"a": "x".repeat(MAX_VALUE_BYTES)}))
            .unwrap()
            .len(),
        MAX_VALUE_BYTES
    );
    let many = template(&"{{a}}".repeat(3000), &[("a", required())]);
    assert_eq!(
        render_problems(&many, json!({"a": "x".repeat(MAX_VALUE_BYTES)})),
        [pair(
            "variables",
            "the rendered text would exceed 65536 bytes"
        )]
    );
    let supplied: Map<String, Value> = (0..40).map(|n| (format!("u{n:02}"), json!("x"))).collect();
    match t.render(&supplied) {
        Err(TemplateError::RenderInvalid(items)) => {
            assert_eq!(items.len(), 21);
            assert_eq!(items[20], issue("", "21 more problems were omitted"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn rendering_is_deterministic() {
    let t = template("{{a}}-{{b}}", &[("a", required()), ("b", optional("z"))]);
    let first = render(&t, json!({"a": "é"})).unwrap();
    assert_eq!(first, "é-z");
    assert_eq!(
        render(&t, json!({"a": "é"})).unwrap().as_bytes(),
        first.as_bytes()
    );
}

#[test]
fn references_parse_strictly() {
    assert_eq!(
        TemplateRef::parse("user:a-b_1").unwrap(),
        TemplateRef::User("a-b_1".into())
    );
    assert_eq!(
        TemplateRef::parse("pack:demo:templates/a.toml").unwrap(),
        TemplateRef::Pack {
            pack_id: "demo".into(),
            path: "templates/a.toml".into()
        }
    );
    for bad in [
        "",
        "user:",
        "user:../x",
        "user:A",
        "user:a/b",
        "pack:demo",
        "pack::x",
        "pack:De:x",
        "pack:demo:",
        "x:y",
    ] {
        assert!(
            matches!(TemplateRef::parse(bad), Err(TemplateError::InvalidRef(_))),
            "{bad}"
        );
    }
}

#[test]
fn the_written_file_round_trips_awkward_text() {
    let text = "line one\r\nsay \"hi\" \\ 'x' {{{{ {{who}}\n\ttail é";
    let original = definition("t", text, &[("who", required())]);
    let bytes = original.to_file_bytes().unwrap();
    let parsed: TemplateDefinition = toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(parsed, original);
}

fn store() -> (TempDir, UserTemplates) {
    let temp = TempDir::new().unwrap();
    let store = UserTemplates::new(temp.path().join("lib/templates"));
    (temp, store)
}

fn user_file(temp: &TempDir, id: &str) -> PathBuf {
    temp.path().join("lib/templates").join(format!("{id}.toml"))
}

#[test]
fn create_writes_one_file_and_the_directory_on_first_write() {
    let (temp, store) = store();
    assert!(store.list().templates.is_empty());
    let created = store
        .create(&definition("portrait", "A {{who}}", &[("who", required())]))
        .unwrap();
    assert_eq!(created.reference, "user:portrait");
    let bytes = fs::read(user_file(&temp, "portrait")).unwrap();
    assert_eq!(created.sha256, sha256_hex(&bytes));
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "id = \"portrait\"\ntitle = \"A title\"\ntext = \"A {{who}}\"\n\n[variables.who]\nrequired = true\n"
    );
    assert_eq!(store.list().templates, std::slice::from_ref(&created));
    assert_eq!(store.get("portrait").unwrap(), created);
}

#[test]
fn create_never_overwrites_and_leaves_no_temporary_files() {
    let (temp, store) = store();
    store.create(&definition("a", "first", &[])).unwrap();
    let before = fs::read(user_file(&temp, "a")).unwrap();
    let error = store.create(&definition("a", "second", &[])).unwrap_err();
    assert!(matches!(error, TemplateError::Exists(id) if id == "a"));
    assert_eq!(fs::read(user_file(&temp, "a")).unwrap(), before);
    let mut names: Vec<String> = fs::read_dir(temp.path().join("lib/templates"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [".templates.lock", "a.toml"]);
}

#[test]
fn create_refuses_an_invalid_definition_and_traversal_ids_without_writing() {
    let (temp, store) = store();
    for id in ["../escape", "a/b", "A", "", ".hidden", "a.toml"] {
        let error = store.create(&definition(id, "x", &[])).unwrap_err();
        assert!(matches!(error, TemplateError::Invalid(_)), "{id}");
    }
    let error = store.create(&definition("ok", "{{x}}", &[])).unwrap_err();
    assert!(matches!(error, TemplateError::Invalid(_)));
    assert!(!temp.path().join("lib").exists());
    assert!(!temp.path().join("escape.toml").exists());
}

#[test]
fn replace_with_the_current_hash_is_atomic_and_with_a_stale_hash_changes_nothing() {
    let (temp, store) = store();
    let created = store.create(&definition("a", "one", &[])).unwrap();
    let replaced = store
        .replace("a", &created.sha256, &definition("a", "two", &[]))
        .unwrap();
    assert_eq!(replaced.text, "two");
    assert_ne!(replaced.sha256, created.sha256);
    let after_replace = fs::read(user_file(&temp, "a")).unwrap();
    assert_eq!(
        after_replace,
        b"id = \"a\"\ntitle = \"A title\"\ntext = \"two\"\n"
    );
    let error = store
        .replace("a", &created.sha256, &definition("a", "three", &[]))
        .unwrap_err();
    assert!(
        matches!(&error, TemplateError::Changed { current_sha256 } if *current_sha256 == replaced.sha256),
        "{error:?}"
    );
    assert_eq!(fs::read(user_file(&temp, "a")).unwrap(), after_replace);
}

#[test]
fn replace_requires_a_well_formed_hash_an_existing_file_and_the_same_id() {
    let (temp, store) = store();
    let created = store.create(&definition("a", "one", &[])).unwrap();
    let before = fs::read(user_file(&temp, "a")).unwrap();
    assert!(matches!(
        store.replace("a", "abc", &definition("a", "two", &[])),
        Err(TemplateError::Invalid(items)) if items[0].key == "expected_sha256"
    ));
    assert!(matches!(
        store.replace("a", &created.sha256, &definition("b", "two", &[])),
        Err(TemplateError::Invalid(items)) if items[0].key == "id"
    ));
    assert!(matches!(
        store.replace("missing", &created.sha256, &definition("missing", "x", &[])),
        Err(TemplateError::NotFound(_))
    ));
    assert!(matches!(
        store.replace("../a", &created.sha256, &definition("../a", "x", &[])),
        Err(TemplateError::Invalid(_)) | Err(TemplateError::InvalidRef(_))
    ));
    assert_eq!(fs::read(user_file(&temp, "a")).unwrap(), before);
}

#[test]
fn a_broken_file_can_be_replaced_with_its_hash() {
    let (temp, store) = store();
    fs::create_dir_all(temp.path().join("lib/templates")).unwrap();
    fs::write(user_file(&temp, "bad"), "not toml [").unwrap();
    let sha = sha256_hex(b"not toml [");
    let fixed = store
        .replace("bad", &sha, &definition("bad", "ok", &[]))
        .unwrap();
    assert_eq!(fixed.text, "ok");
}

#[test]
fn delete_needs_the_current_hash() {
    let (temp, store) = store();
    let created = store.create(&definition("a", "one", &[])).unwrap();
    assert!(matches!(
        store.delete("a", &"0".repeat(64)),
        Err(TemplateError::Changed { .. })
    ));
    assert!(user_file(&temp, "a").exists());
    store.delete("a", &created.sha256).unwrap();
    assert!(!user_file(&temp, "a").exists());
    assert!(matches!(
        store.delete("a", &created.sha256),
        Err(TemplateError::NotFound(_))
    ));
    assert!(matches!(
        store.delete("../a", &created.sha256),
        Err(TemplateError::InvalidRef(_))
    ));
}

#[test]
fn listing_reports_unusable_files_as_diagnostics_only() {
    let (temp, store) = store();
    store.create(&definition("good", "x", &[])).unwrap();
    let directory = temp.path().join("lib/templates");
    fs::write(
        directory.join("broken.toml"),
        "id = 'broken'\ntitle = 'T'\ntext = '{{x}}'\n",
    )
    .unwrap();
    fs::write(
        directory.join("other.toml"),
        "id = 'different'\ntitle = 'T'\ntext = 'x'\n",
    )
    .unwrap();
    fs::write(directory.join("Bad Name.toml"), "x").unwrap();
    fs::write(directory.join("notes.txt"), "ignored").unwrap();
    fs::write(directory.join(".hidden.toml"), "ignored").unwrap();
    let listing = store.list();
    assert_eq!(listing.templates.len(), 1);
    assert_eq!(listing.templates[0].id, "good");
    let found: Vec<(Option<String>, String)> = listing
        .diagnostics
        .iter()
        .map(|d| (d.reference.clone(), d.error.clone()))
        .collect();
    assert_eq!(
        found,
        [
            (None, "the file name is not a valid template id; rename it to '<id>.toml' with a lower-case id".into()),
            (
                Some("user:broken".into()),
                "text: uses the undeclared placeholder '{{x}}'; declare it under [variables.x]".into()
            ),
            (Some("user:other".into()), "id: must equal the file name 'other'".into()),
        ]
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_template_is_not_followed() {
    let (temp, store) = store();
    store.create(&definition("a", "x", &[])).unwrap();
    let outside = temp.path().join("outside.toml");
    fs::write(&outside, "id = 'link'\ntitle = 'T'\ntext = 'secret'\n").unwrap();
    std::os::unix::fs::symlink(&outside, user_file(&temp, "link")).unwrap();
    let listing = store.list();
    assert_eq!(listing.templates.len(), 1);
    assert_eq!(listing.diagnostics.len(), 1);
    assert!(listing.diagnostics[0].error.contains("not a regular file"));
    assert!(store.get("link").is_err());
}

#[test]
fn concurrent_creates_of_one_id_have_exactly_one_winner() {
    let (_temp, store) = store();
    let winners: usize = std::thread::scope(|scope| {
        (0..8)
            .map(|n| {
                let store = store.clone();
                scope.spawn(move || {
                    store
                        .create(&definition("race", &format!("v{n}"), &[]))
                        .is_ok()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum()
    });
    assert_eq!(winners, 1);
}

#[test]
fn a_hash_pinned_replace_has_one_winner_among_concurrent_writers() {
    let (_temp, store) = store();
    let created = store.create(&definition("a", "start", &[])).unwrap();
    let winners: usize = std::thread::scope(|scope| {
        (0..8)
            .map(|n| {
                let store = store.clone();
                let sha = created.sha256.clone();
                scope.spawn(move || {
                    store
                        .replace("a", &sha, &definition("a", &format!("v{n}"), &[]))
                        .is_ok()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| usize::from(handle.join().unwrap()))
            .sum()
    });
    assert_eq!(winners, 1);
}
