use genaimage_api::{client::Client, PackRequirementStatus, PackValidationStatus};

use crate::{
    output::{Error, Output, Status},
    PacksCommand,
};

pub(crate) async fn run(
    client: &Client,
    output: Output,
    command: &PacksCommand,
) -> Result<Status, Error> {
    match command {
        PacksCommand::List => {
            let listing = client.packs().await?;
            output.write(&listing, |out| {
                if listing.packs.is_empty() {
                    writeln!(out, "No workflow packs found.")?;
                }
                for pack in &listing.packs {
                    writeln!(
                        out,
                        "{}  {} ({} workflows, {} templates)",
                        pack.id,
                        pack.version,
                        pack.workflow_ids.len(),
                        pack.templates.len()
                    )?;
                    for diagnostic in &pack.diagnostics {
                        writeln!(out, "  Problem: {diagnostic}")?;
                    }
                }
                for diagnostic in &listing.diagnostics {
                    writeln!(out, "Skipped {}: {}", diagnostic.path, diagnostic.error)?;
                }
                Ok(())
            })?;
            Ok(Status::Success)
        }
        PacksCommand::Validate { id } => {
            let validation = client.validate_pack(id).await?;
            output.write(&validation, |out| {
                writeln!(out, "Pack {}: {:?}", validation.pack_id, validation.status)?;
                if let Some(reason) = &validation.reason {
                    writeln!(out, "  {reason}")?;
                }
                for node in &validation.node_classes {
                    if node.status != PackRequirementStatus::Available {
                        writeln!(out, "  node class {}: {:?}", node.class, node.status)?;
                    }
                }
                for model in &validation.models {
                    if model.status != PackRequirementStatus::Available {
                        writeln!(
                            out,
                            "  model {}/{}: {:?}{}",
                            model.folder,
                            model.name,
                            model.status,
                            model
                                .reason
                                .as_deref()
                                .map(|reason| format!(" ({reason})"))
                                .unwrap_or_default()
                        )?;
                    }
                }
                Ok(())
            })?;
            Ok(if validation.status == PackValidationStatus::Ok {
                Status::Success
            } else {
                Status::NegativeResult
            })
        }
    }
}
