mod binary_crates;
mod rustup;

use crate::workspace::Workspace;
use anyhow::{bail, Error};
use binary_crates::BinaryCrate;
use log::info;
use rustup::Rustup;
use std::env::consts::EXE_SUFFIX;
use std::path::PathBuf;

pub(crate) static RUSTUP: Rustup = Rustup;

#[cfg(feature = "unstable-toolchain-ci")]
pub(crate) static RUSTUP_TOOLCHAIN_INSTALL_MASTER: BinaryCrate = BinaryCrate {
    crate_name: "rustup-toolchain-install-master",
    binary: "rustup-toolchain-install-master",
    cargo_subcommand: None,
};

pub(crate) static GIT_CREDENTIAL_NULL: BinaryCrate = BinaryCrate {
    crate_name: "git-credential-null",
    binary: "git-credential-null",
    cargo_subcommand: None,
};

static INSTALLABLE_TOOLS: &[&dyn Tool] = &[
    &RUSTUP,
    #[cfg(feature = "unstable-toolchain-ci")]
    &RUSTUP_TOOLCHAIN_INSTALL_MASTER,
    &GIT_CREDENTIAL_NULL,
];

trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_installed(&self, workspace: &Workspace) -> Result<bool, Error>;
    fn install(&self, workspace: &Workspace, fast_install: bool) -> Result<(), Error>;
    fn update(&self, workspace: &Workspace, fast_install: bool) -> Result<(), Error>;

    fn binary_path(&self, workspace: &Workspace) -> PathBuf {
        crate::utils::normalize_path(&workspace.cargo_home().join("bin").join(format!(
            "{}{}",
            self.name(),
            EXE_SUFFIX
        )))
    }
}

pub(crate) fn install(workspace: &Workspace, fast_install: bool) -> Result<(), Error> {
    for tool in INSTALLABLE_TOOLS {
        if tool.is_installed(workspace)? {
            info!("tool {} is installed, trying to update it", tool.name());
            tool.update(workspace, fast_install)?;
        } else {
            info!("tool {} is missing, installing it", tool.name());
            tool.install(workspace, fast_install)?;

            if !tool.is_installed(workspace)? {
                bail!("tool {} is still missing after install", tool.name());
            }
        }
    }

    Ok(())
}
