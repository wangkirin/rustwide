use crate::build::BuildDirectory;
use crate::cmd::{Command, SandboxImage};
use crate::inside_docker::CurrentContainer;
use crate::Toolchain;
use anyhow::{Context, Error, Result};
use log::info;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[cfg(windows)]
static DEFAULT_SANDBOX_IMAGE: &str = "rustops/crates-build-env-windows";

#[cfg(not(windows))]
static DEFAULT_SANDBOX_IMAGE: &str = "ghcr.io/rust-lang/crates-build-env/linux";

const DEFAULT_COMMAND_TIMEOUT: Option<Duration> = Some(Duration::from_secs(15 * 60));
const DEFAULT_COMMAND_NO_OUTPUT_TIMEOUT: Option<Duration> = None;

static DEFAULT_RUSTUP_PROFILE: &str = "minimal";

/// Builder of a [`Workspace`](struct.Workspace.html).
pub struct WorkspaceBuilder {
    user_agent: String,
    path: PathBuf,
    sandbox_image: Option<SandboxImage>,
    command_timeout: Option<Duration>,
    command_no_output_timeout: Option<Duration>,
    fetch_registry_index_during_builds: bool,
    running_inside_docker: bool,
    fast_init: bool,
    rustup_profile: String,
}

impl WorkspaceBuilder {
    /// Create a new builder.
    ///
    /// The provided path will be the home of the workspace, containing all the data generated by
    /// rustwide (including state and caches).
    pub fn new(path: &Path, user_agent: &str) -> Self {
        Self {
            user_agent: user_agent.into(),
            path: path.into(),
            sandbox_image: None,
            command_timeout: DEFAULT_COMMAND_TIMEOUT,
            command_no_output_timeout: DEFAULT_COMMAND_NO_OUTPUT_TIMEOUT,
            fetch_registry_index_during_builds: true,
            running_inside_docker: false,
            fast_init: false,
            rustup_profile: DEFAULT_RUSTUP_PROFILE.into(),
        }
    }

    /// Override the image used for sandboxes.
    ///
    /// By default rustwide will use the [ghcr.io/rust-lang/crates-build-env/linux-micro] image on
    /// Linux systems, and [rustops/crates-build-env-windows] on Windows systems. Those images
    /// contain dependencies to build a large amount of crates.
    ///
    /// [ghcr.io/rust-lang/crates-build-env/linux-micro]: https://github.com/orgs/rust-lang/packages/container/package/crates-build-env/linux-micro
    /// [rustops/crates-build-env-windows]: https://hub.docker.com/r/rustops/crates-build-env-windows
    pub fn sandbox_image(mut self, image: SandboxImage) -> Self {
        self.sandbox_image = Some(image);
        self
    }

    /// Set the default timeout of [`Command`](cmd/struct.Command.html), which can be overridden
    /// with the [`Command::timeout`](cmd/struct.Command.html#method.timeout) method. To disable
    /// the timeout set its value to `None`. By default the timeout is 15 minutes.
    pub fn command_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.command_timeout = timeout;
        self
    }

    /// Set the default no output timeout of [`Command`](cmd/struct.Command.html), which can be
    /// overridden with the
    /// [`Command::no_output_timeout`](cmd/struct.Command.html#method.no_output_timeout) method. To
    /// disable the timeout set its value to `None`. By default it's disabled.
    pub fn command_no_output_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.command_no_output_timeout = timeout;
        self
    }

    /// Enable or disable fast workspace initialization (disabled by default).
    ///
    /// Fast workspace initialization will change the initialization process to prefer
    /// initialization speed to runtime performance, for example by installing the tools rustwide
    /// needs in debug mode instead of release mode. It's not recommended to enable fast workspace
    /// initialization with production workloads, but it can help in CIs or other automated testing
    /// scenarios.
    pub fn fast_init(mut self, enable: bool) -> Self {
        self.fast_init = enable;
        self
    }

    /// Enable or disable fetching the registry's index during each build (enabled by default).
    ///
    /// When this option is disabled the index will only be fetched when the workspace is
    /// initialized, and no following build do that again. It's useful to disable it when you need
    /// to build a lot of crates in a batch, but having the option disabled might cause trouble if
    /// you need to build recently published crates, as they might be missing from the cached
    /// index.
    #[cfg(any(feature = "unstable", doc))]
    #[cfg_attr(docs_rs, doc(cfg(feature = "unstable")))]
    pub fn fetch_registry_index_during_builds(mut self, enable: bool) -> Self {
        self.fetch_registry_index_during_builds = enable;
        self
    }

    /// Enable or disable support for running Rustwide itself inside Docker (disabled by default).
    ///
    /// When support is enabled Rustwide will try to detect whether it's actually running inside a
    /// Docker container during initialization, and in that case it will adapt itself. This is
    /// needed because starting a sibling container from another one requires mount sources to be
    /// remapped to the real directory on the host.
    ///
    /// Other than enabling support for it, to run Rustwide inside Docker your container needs to
    /// meet these requirements:
    ///
    /// * The Docker socker (`/var/run/docker.sock`) needs to be mounted inside the container.
    /// * The workspace directory must be either mounted from the host system or in a child
    ///   directory of a mount from the host system. Workspaces created inside the container are
    ///   not supported.
    pub fn running_inside_docker(mut self, inside: bool) -> Self {
        self.running_inside_docker = inside;
        self
    }

    /// Name of the rustup profile used when installing toolchains. The default is `minimal`.
    pub fn rustup_profile(mut self, profile: &str) -> Self {
        self.rustup_profile = profile.into();
        self
    }

    /// Initialize the workspace. This will create all the necessary local files and fetch the rest from the network. It's
    /// not unexpected for this method to take minutes to run on slower network connections.
    pub fn init(self) -> Result<Workspace, Error> {
        std::fs::create_dir_all(&self.path).with_context(|| {
            format!(
                "failed to create workspace directory: {}",
                self.path.display()
            )
        })?;

        crate::utils::file_lock(&self.path.join("lock"), "initialize the workspace", || {
            let sandbox_image = if let Some(img) = self.sandbox_image {
                img
            } else {
                SandboxImage::remote(DEFAULT_SANDBOX_IMAGE)?
            };

            let mut agent = attohttpc::Session::new();
            agent.header(http::header::USER_AGENT, self.user_agent);

            let mut ws = Workspace {
                inner: Arc::new(WorkspaceInner {
                    http: agent,
                    path: self.path,
                    sandbox_image,
                    command_timeout: self.command_timeout,
                    command_no_output_timeout: self.command_no_output_timeout,
                    fetch_registry_index_during_builds: self.fetch_registry_index_during_builds,
                    current_container: None,
                    rustup_profile: self.rustup_profile,
                }),
            };

            if self.running_inside_docker {
                let container = CurrentContainer::detect(&ws)?;
                Arc::get_mut(&mut ws.inner).unwrap().current_container = container;
            }

            ws.init(self.fast_init)?;
            Ok(ws)
        })
    }
}

struct WorkspaceInner {
    http: attohttpc::Session,
    path: PathBuf,
    sandbox_image: SandboxImage,
    command_timeout: Option<Duration>,
    command_no_output_timeout: Option<Duration>,
    fetch_registry_index_during_builds: bool,
    current_container: Option<CurrentContainer>,
    rustup_profile: String,
}

/// Directory on the filesystem containing rustwide's state and caches.
///
/// Use [`WorkspaceBuilder`](struct.WorkspaceBuilder.html) to create a new instance of it.
pub struct Workspace {
    inner: Arc<WorkspaceInner>,
}

impl Workspace {
    /// Open a named build directory inside the workspace.
    pub fn build_dir(&self, name: &str) -> BuildDirectory {
        BuildDirectory::new(
            Workspace {
                inner: self.inner.clone(),
            },
            name,
        )
    }

    /// Remove all the contents of all the build directories, freeing disk space.
    pub fn purge_all_build_dirs(&self) -> Result<(), Error> {
        let dir = self.builds_dir();
        if dir.exists() {
            crate::utils::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// Remove all the contents of the caches in the workspace, freeing disk space.
    pub fn purge_all_caches(&self) -> Result<(), Error> {
        let mut paths = vec![
            self.cache_dir(),
            self.cargo_home().join("git"),
            self.cargo_home().join("registry").join("src"),
            self.cargo_home().join("registry").join("cache"),
        ];

        for index in std::fs::read_dir(self.cargo_home().join("registry").join("index"))? {
            let index = index?;
            if index.file_type()?.is_dir() {
                paths.push(index.path().join(".cache"));
            }
        }

        for path in &paths {
            if path.exists() {
                crate::utils::remove_dir_all(path)?;
            }
        }

        Ok(())
    }

    /// Return a list of all the toolchains present in the workspace.
    ///
    /// # Example
    ///
    /// This code snippet removes all the installed toolchains except the main one:
    ///
    /// ```no_run
    /// # use rustwide::{WorkspaceBuilder, Toolchain};
    /// # use std::error::Error;
    /// # fn main() -> Result<(), Box<dyn Error>> {
    /// # let workspace = WorkspaceBuilder::new("".as_ref(), "").init()?;
    /// let main_toolchain = Toolchain::dist("stable");
    /// for installed in &workspace.installed_toolchains()? {
    ///     if *installed != main_toolchain {
    ///         installed.uninstall(&workspace)?;
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn installed_toolchains(&self) -> Result<Vec<Toolchain>, Error> {
        crate::toolchain::list_installed_toolchains(&self.rustup_home())
    }

    pub(crate) fn http_client(&self) -> &attohttpc::Session {
        &self.inner.http
    }

    pub(crate) fn cargo_home(&self) -> PathBuf {
        self.inner.path.join("cargo-home")
    }

    pub(crate) fn rustup_home(&self) -> PathBuf {
        self.inner.path.join("rustup-home")
    }

    pub(crate) fn cache_dir(&self) -> PathBuf {
        self.inner.path.join("cache")
    }

    pub(crate) fn builds_dir(&self) -> PathBuf {
        self.inner.path.join("builds")
    }

    pub(crate) fn sandbox_image(&self) -> &SandboxImage {
        &self.inner.sandbox_image
    }

    pub(crate) fn default_command_timeout(&self) -> Option<Duration> {
        self.inner.command_timeout
    }

    pub(crate) fn default_command_no_output_timeout(&self) -> Option<Duration> {
        self.inner.command_no_output_timeout
    }

    pub(crate) fn fetch_registry_index_during_builds(&self) -> bool {
        self.inner.fetch_registry_index_during_builds
    }

    pub(crate) fn current_container(&self) -> Option<&CurrentContainer> {
        self.inner.current_container.as_ref()
    }

    pub(crate) fn rustup_profile(&self) -> &str {
        &self.inner.rustup_profile
    }

    fn init(&self, fast_init: bool) -> Result<(), Error> {
        info!("installing tools required by rustwide");
        crate::tools::install(self, fast_init)?;
        if !self.fetch_registry_index_during_builds() {
            info!("updating the local crates.io registry clone");
            self.update_cratesio_registry()?;
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)] // hopefully we could actually catch the error here at some point
    fn update_cratesio_registry(&self) -> Result<(), Error> {
        // This nop cargo command is to update the registry so we don't have to do it for each
        // crate.  using `install` is a temporary solution until
        // https://github.com/rust-lang/cargo/pull/5961 is ready

        let _ = Command::new(self, Toolchain::MAIN.cargo())
            .args(&["install", "lazy_static"])
            .no_output_timeout(None)
            .run();

        // ignore the error untill https://github.com/rust-lang/cargo/pull/5961 is ready
        Ok(())
    }
}
