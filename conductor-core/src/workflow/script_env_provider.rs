use std::collections::HashMap;

use runkon_flow::traits::run_context::RunContext;
use runkon_flow::traits::script_env_provider::ScriptEnvProvider;

/// Conductor-specific script env provider.
///
/// Composes a `PATH` that prepends `conductor_bin_dir` and any extra plugin
/// directories in front of the inherited `PATH`, then delegates env building
/// to the caller's script executor.
pub(crate) struct ConductorScriptEnvProvider {
    conductor_bin_dir: Option<std::path::PathBuf>,
    extra_plugin_dirs: Vec<String>,
}

impl ConductorScriptEnvProvider {
    pub(crate) fn new(
        conductor_bin_dir: Option<std::path::PathBuf>,
        extra_plugin_dirs: Vec<String>,
    ) -> Self {
        Self {
            conductor_bin_dir,
            extra_plugin_dirs,
        }
    }
}

impl ScriptEnvProvider for ConductorScriptEnvProvider {
    fn env(&self, _ctx: &dyn RunContext) -> HashMap<String, String> {
        let mut env = HashMap::new();
        if let Some(ref bin_dir) = self.conductor_bin_dir {
            let existing = std::env::var("PATH").unwrap_or_default();
            let mut parts = vec![bin_dir.display().to_string()];
            parts.extend(self.extra_plugin_dirs.iter().cloned());
            if !existing.is_empty() {
                parts.push(existing);
            }
            env.insert("PATH".to_string(), parts.join(":"));
        }
        env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopCtx;
    impl RunContext for NoopCtx {
        fn injected_variables(&self) -> HashMap<&'static str, String> {
            HashMap::new()
        }
        fn working_dir(&self) -> &std::path::Path {
            std::path::Path::new("/tmp")
        }
        fn repo_path(&self) -> &std::path::Path {
            std::path::Path::new("/tmp")
        }
    }

    #[test]
    fn test_env_empty_when_no_bin_dir() {
        let provider = ConductorScriptEnvProvider::new(None, vec![]);
        let env = provider.env(&NoopCtx);
        assert!(env.is_empty(), "expected empty env when no bin_dir is set");
    }

    #[test]
    fn test_env_path_starts_with_bin_dir() {
        let bin_dir = std::path::PathBuf::from("/usr/local/bin/conductor-dir");
        let provider = ConductorScriptEnvProvider::new(Some(bin_dir), vec![]);
        let env = provider.env(&NoopCtx);
        let path = env.get("PATH").expect("PATH should be set");
        assert!(
            path.starts_with("/usr/local/bin/conductor-dir"),
            "PATH should start with conductor_bin_dir, got: {path}"
        );
    }

    #[test]
    fn test_env_path_includes_plugin_dirs() {
        let bin_dir = std::path::PathBuf::from("/bin/conductor");
        let provider = ConductorScriptEnvProvider::new(
            Some(bin_dir),
            vec!["/opt/plugins".to_string(), "/home/user/plugins".to_string()],
        );
        let env = provider.env(&NoopCtx);
        let path = env.get("PATH").expect("PATH should be set");
        let parts: Vec<&str> = path.splitn(4, ':').collect();
        assert_eq!(parts[0], "/bin/conductor");
        assert_eq!(parts[1], "/opt/plugins");
        assert_eq!(parts[2], "/home/user/plugins");
    }
}
