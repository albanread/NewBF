# Engineering Journal

A running lab notebook for NewBF. Commits record **what** changed; this records
the **why** — the reasoning behind decisions, the dead ends, the bugs and how we
chased them, the "aha" moments, and the constraints we discovered. The stuff
that's expensive to re-derive and invisible in a diff.

## Convention

- One file per work session, named `YYYY-MM-DD.md` (append a suffix if a day has
  more than one: `-2`, `-3`).
- Write for a future reader (often us) who has the code but not the context.
- Favour **why over what**: decisions + alternatives rejected + challenges +
  how they were solved + insights worth keeping. Skip blow-by-blow narration.
- Cross-reference commits by short hash and docs by name where useful.
- It's fine to be candid about risks taken and things still uncertain.

## Index

- [2026-05-31](2026-05-31.md) — Control-flow completion; the no-GC data-type
  model + GC escape-hatch direction; the value-struct layout sprint.
