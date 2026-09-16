use sunoto_system::{
    ActionCandidate, ActionExecutor, ActionResult, ApplicationResolver, ResolvedSystemAction,
    SystemOperationError,
};

#[derive(Default)]
pub struct SystemPlatform;

impl SystemPlatform {
    pub fn with_search_roots(_search_roots: Vec<std::path::PathBuf>) -> Self {
        Self
    }
}

impl ApplicationResolver for SystemPlatform {
    fn application_candidates(&mut self) -> Result<Vec<ActionCandidate>, SystemOperationError> {
        Err(SystemOperationError::new(
            "Linux application discovery is unavailable on this platform",
        ))
    }
}

impl ActionExecutor for SystemPlatform {
    fn execute(
        &mut self,
        _action: &ResolvedSystemAction,
    ) -> Result<ActionResult, SystemOperationError> {
        Err(SystemOperationError::new(
            "Linux application launching is unavailable on this platform",
        ))
    }
}
