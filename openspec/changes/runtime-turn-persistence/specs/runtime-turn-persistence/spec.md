## ADDED Requirements

### Requirement: Runtime turn assistant content is persisted on completion

When a runtime (claude/OMP) dialog turn ends with `TurnEnd { StopReason::Completed }`, the assistant text AND thinking streamed during the turn MUST be persisted to the session store so they are visible after the session is reloaded. The runtime event loop SHALL accumulate the streamed `TextDelta` and `ThinkingDelta` content and pass them to `complete_dialog_turn`, which injects them into a synthetic model round (text into `text_items`, thinking into `thinking_items`) when the turn has no existing assistant text.

#### Scenario: Completed runtime turn survives reload
- **WHEN** a runtime turn streams assistant text and thinking and ends with `StopReason::Completed`
- **THEN** the accumulated text SHALL be written to `model_rounds[].text_items` and the accumulated thinking to `model_rounds[].thinking_items`, and both SHALL be present when the session is reloaded

#### Scenario: Completed runtime turn with no content produces no empty round
- **WHEN** a runtime turn ends with `StopReason::Completed` but streamed neither text nor thinking
- **THEN** no synthetic empty model round SHALL be injected

#### Scenario: Completed runtime turn with thinking but no text
- **WHEN** a runtime turn streams only thinking (no assistant text) and ends with `StopReason::Completed`
- **THEN** a model round SHALL be injected containing the thinking in `thinking_items`, with empty `text_items`

### Requirement: Runtime turn partial content is persisted on cancellation

When a runtime dialog turn is cancelled after streaming partial assistant text and/or thinking, that content MUST be persisted so it is visible after reload. `cancel_dialog_turn` SHALL accept optional `partial_text` and `partial_thinking` and inject them into `model_rounds` only when the turn has no existing assistant text (so the bitfun path, which passes `None` for both, is unaffected).

#### Scenario: Cancelled runtime turn preserves partial content
- **WHEN** a runtime turn streams partial text and thinking and is then cancelled
- **THEN** the partial text and thinking SHALL be persisted to `model_rounds` with turn status `Cancelled`, and SHALL be present after reload

#### Scenario: Cancellation before any content streamed
- **WHEN** a runtime turn is cancelled before any text or thinking is streamed (e.g. D8 pre-prompt or during prompt() error)
- **THEN** the turn status SHALL be set to `Cancelled` and no empty model round SHALL be injected

#### Scenario: bitfun cancellation is unaffected
- **WHEN** a bitfun turn is cancelled and `cancel_dialog_turn` is called with `partial_text = None` and `partial_thinking = None`
- **THEN** the existing bitfun cancellation behaviour SHALL be unchanged (no new round injected)

### Requirement: Runtime turn partial content is persisted on failure

When a runtime dialog turn fails after streaming partial assistant text and/or thinking, that content MUST be persisted so it is visible after reload. `fail_dialog_turn` SHALL accept optional `partial_text` and `partial_thinking` and inject them into `model_rounds` only when the turn has no existing assistant text.

#### Scenario: Failed runtime turn preserves generated content
- **WHEN** a runtime turn streams text and/or thinking and then ends with `RuntimeEvent::Error` or a non-Completed/Aborted `StopReason`
- **THEN** the generated text and thinking SHALL be persisted to `model_rounds` with turn status `Error`, and SHALL be present after reload

#### Scenario: bitfun failure is unaffected
- **WHEN** a bitfun turn fails and `fail_dialog_turn` is called with `partial_text = None` and `partial_thinking = None`
- **THEN** the existing bitfun failure behaviour SHALL be unchanged (no new round injected)

### Requirement: Partial-content injection is idempotent against existing assistant text

The injection of `partial_text` / `partial_thinking` MUST be guarded so it never overwrites or duplicates content that already exists in `model_rounds`. Injection SHALL occur only when the turn currently has no non-empty assistant text item.

#### Scenario: Turn already has assistant text
- **WHEN** a dialog turn already contains a non-empty assistant `text_item` and a persist path is called with `partial_text`/`partial_thinking` set
- **THEN** no additional round SHALL be injected and the existing content SHALL be preserved unchanged
