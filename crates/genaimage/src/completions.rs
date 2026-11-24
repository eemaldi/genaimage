use std::io;

use clap::CommandFactory;
use clap_complete::{generate as generate_completions, Shell};

pub(crate) fn generate(shell: Shell) {
    let mut command = crate::Cli::command();
    generate_completions(shell, &mut command, "genaimage", &mut io::stdout().lock());
}
