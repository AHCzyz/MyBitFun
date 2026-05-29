## ADDED Requirements

### Requirement: Runtime turn assistant text is persisted on completion

When a runtime (claude/OMP) dialog turn ends with `TurnEnd { StopReason::Completed }`, the assistant text streamed during the turn MUST be persisted to the session store so it is visible after the session is reloaded. The runtime event loop SHALL accumulate the `TextDelta` content it streams and pass it to `complete_dialog_turn` so the existing `has_assistant_text` fallback persists it as a model round.

#### Scenario: Completed runtime turn survives reload
- **WHEN** a runtime turn streams assistant text and ends with `StopReason::Completed`
- **THEN** the accumulated assistant text SHALL be written to the dialog turn's `model_rounds` and SHALL be present when the session is reloaded

#### Scenario: Completed runtime turn with no text produces no empty round
- **WHEN** a runtime turn ends with `StopReason::Completed` but streamed no assistant text
- **THEN** no synthetic empty model round SHALL be injected

### Requirement: Runtime turn partial text is persisted on cancellation

When a runtime dialog turn is cancelled after streaming partial assistant text, the partial text MUST be persisted so it is visible after reload. `cancel_dialog_turn` SHALL accept an optional `partial_text` and inject it into `model_rounds` only when the turn has no existing assistant text (so the bitfun path, which passes no text, is unaffected).

#### Scenario: Cancelled runtime turn preserves partial text
- **WHEN** a runtime turn streams partial assistant text and is then cancelled
- **THEN** the partial text SHALL be persisted to `model_rounds` with turn status `Cancelled`, and SHALL be present after reload

#### Scenario: Cancellation before any text streamed
- **WHEN** a runtime turn is cancelled before any assistant text is streamed (e.g. D8 pre-prompt or during prompt() error)
- **THEN** the turn status SHALL be set to `Cancelled` and no empty model round SHALL be injected

#### Scenario: bitfun cancellation is unaffected
- **WHEN** a bitfun turn is cancelled and `cancel_dialog_turn` is called with `partial_text = None`
- **THEN** the existing bitfun cancellation behaviour SHALL be unchanged (no new round injected)

### Requirement: Runtime turn partial text is persisted on failure

When a runtime dialog turn fails after streaming partial assistant text, the partial text MUST be persisted so it is visible after reload. `fail_dialog_turn` SHALL accept an optional `partial_text` and inject it into `model_rounds` only when the turn has no existing assistant text.

#### Scenario: Failed runtime turn preserves generated text
- **WHEN** a runtime turn streams assistant text and then ends with `RuntimeEvent::Error` or a non-Completed/Aborted `StopReason`
- **THEN** the generated text SHALL be persisted to `model_rounds` with turn status `Error`, and SHALL be present after reload

#### Scenario: bitfun failure is unaffected
- **WHEN** a bitfun turn fails and `fail_dialog_turn` is called with `partial_text = None`
- **THEN** the existing bitfun failure behaviour SHALL be unchanged (no new round injected)

### Requirement: Partial-text injection is idempotent against existing assistant text

The injection of `partial_text` MUST be guarded so it never overwrites or duplicates assistant text that already exists in `model_rounds`. Injection SHALL occur only when the turn currently has no non-empty assistant text item.

#### Scenario: Turn already has assistant text
- **WHEN** a dialog turn already contains a non-empty assistant `text_item` and a persist path is called with `partial_text = Some(...)`
- **THEN** no additional round SHALL be injected and the existing content SHALL be preserved unchanged
