use genaimage_api::{client::Client, TemplateRenderRequest};
use serde_json::Value;

use crate::{
    output::{Error, Output, Status},
    TemplatesCommand,
};

fn parse_variables(pairs: &[String]) -> Result<std::collections::BTreeMap<String, Value>, Error> {
    let mut variables = std::collections::BTreeMap::new();
    for pair in pairs {
        let (name, value) = pair
            .split_once('=')
            .ok_or_else(|| Error::Input(format!("--var '{pair}' must be NAME=VALUE")))?;
        if variables
            .insert(name.to_owned(), Value::String(value.to_owned()))
            .is_some()
        {
            return Err(Error::Input(format!("--var '{name}' was given twice")));
        }
    }
    Ok(variables)
}

pub(crate) async fn run(
    client: &Client,
    output: Output,
    command: &TemplatesCommand,
) -> Result<Status, Error> {
    match command {
        TemplatesCommand::List => {
            let listing = client.templates().await?;
            output.write(&listing, |out| {
                if listing.templates.is_empty() {
                    writeln!(out, "No templates found.")?;
                }
                for template in &listing.templates {
                    writeln!(out, "{}  {}", template.reference, template.title)?;
                }
                for diagnostic in &listing.diagnostics {
                    writeln!(out, "Skipped {}: {}", diagnostic.path, diagnostic.error)?;
                }
                Ok(())
            })?;
            Ok(Status::Success)
        }
        TemplatesCommand::Render {
            reference,
            variables,
        } => {
            let variables = parse_variables(variables)?;
            let template = client.template(reference).await?;
            let rendered = client
                .render_template(&TemplateRenderRequest {
                    reference: template.reference,
                    sha256: template.sha256,
                    variables,
                })
                .await?;
            output.write(&rendered, |out| write!(out, "{}", rendered.text))?;
            Ok(Status::Success)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_split_on_the_first_equals_and_keep_empty_values() {
        let found = parse_variables(&["a=1=2".into(), "b=".into()])
            .ok()
            .unwrap();
        assert_eq!(found["a"], "1=2");
        assert_eq!(found["b"], "");
        assert!(parse_variables(&["nope".into()]).is_err());
        assert!(parse_variables(&["a=1".into(), "a=2".into()]).is_err());
    }
}
