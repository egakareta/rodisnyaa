## Test Implementation Rules

Tests exist to detect regressions in externally observable behavior.

DO NOT write tests whose primary assertion is about implementation topology,
wiring, delegation, or internal structure.

Forbidden unless explicitly requested:

- asserting that function A calls function B
- asserting exact call counts on internal collaborators
- mocking the unit under test's own internal modules
- asserting private/internal state
- testing that a mock returns the value configured on that mock
- snapshotting implementation details
- tests that would fail after a behavior-preserving refactor

Prefer, in order:

1. existing tests that already cover the behavior
2. extending an existing behavioral test
3. a new test exercising the public API / observable output
4. integration tests using real collaborators
5. mocks only at true external boundaries (network, filesystem, clock, etc.)

Before adding ANY test, ask:

"If the implementation were completely rewritten while preserving externally
observable behavior, should this test still pass?"

If NO, do not write the test.

A test must be capable of failing for a plausible regression in user-visible
or contractually specified behavior.

If a test seems difficult to implement, stop work and ask me to clarify
if I want to proceed with a complex harness to properly test the behavior.

## UI / Game Testing Rules

DO NOT use screenshots, image capture, `view_image`, visual inspection,
computer-vision analysis, or screenshot-based reasoning to validate the game.

Do not take screenshots unless I explicitly ask you to.

Do not infer UI correctness, rendering correctness, or behavior from images.

Prefer deterministic, machine-readable validation:

- inspect source code and runtime state
- run automated tests
- inspect logs / console output
- query the scene/object/entity state
- use engine debugging APIs or scripts
- use DOM/accessibility data for web-based UI where applicable
- add temporary instrumentation or assertions when necessary

When visual appearance cannot be verified without vision, DO NOT guess.
State exactly what cannot be verified automatically and ask me to verify that
specific visual detail manually.

Never treat a screenshot as evidence that an implementation is correct.
