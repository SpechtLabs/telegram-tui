//! First-run telemetry consent screen state. See docs/architecture.md §4.6.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentChoice {
    Enable,
    Disable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsentState {
    pub selected: ConsentChoice, // Enable preselected (spec §13.5)
    pub acknowledged: bool,
}
