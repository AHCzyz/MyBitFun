## ADDED Requirements

### Requirement: Runtime selector component
The system SHALL provide a UI component that displays available runtimes and allows the user to select one for the current or new session.

#### Scenario: Displays registered runtimes
- **WHEN** the runtime selector is rendered
- **THEN** it lists all runtimes from `RuntimeRegistry` with display name, description, and health status

#### Scenario: Healthy runtime shown as available
- **WHEN** a runtime's `health_check()` returns Ok
- **THEN** the runtime is shown as selectable (not grayed out)

#### Scenario: Unhealthy runtime shown as unavailable
- **WHEN** a runtime's `health_check()` returns an error
- **THEN** the runtime is grayed out with the error reason displayed (e.g. "omp not found in PATH", "ANTHROPIC_API_KEY not set")

### Requirement: Default runtime selection
The system SHALL select the default runtime automatically based on availability: OMP if installed, otherwise BitFun native.

#### Scenario: OMP installed becomes default
- **WHEN** OMP health check passes
- **THEN** new sessions default to OMP runtime

#### Scenario: No external runtime falls back to BitFun
- **WHEN** neither OMP nor Claude health check passes
- **THEN** new sessions default to BitFun native runtime

### Requirement: Runtime selection persists per session
The system SHALL store the selected runtime ID as part of session configuration.

#### Scenario: Resume session uses stored runtime
- **WHEN** a session created with `runtime_id: "omp"` is resumed
- **THEN** the OMP adapter is used for continued interaction

#### Scenario: Switching runtime creates new session with confirmation
- **WHEN** user changes the runtime selector while in an active session
- **THEN** the system prompts for confirmation, then creates a new session with the selected runtime
