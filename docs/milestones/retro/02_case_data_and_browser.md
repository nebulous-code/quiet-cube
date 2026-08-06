# Milestone 2 — Case Data & Browser

Detailed design + story list for M2. Scope is set in `docs/milestones/README.md`. M1 is the precedent for structure and ticket sizing. Authoritative spec is `docs/Cube_Practice_Design_Doc.md` §2, §3 (`puzzle_types`, `solve_stages`, `cases`, `user_case_settings`), §6 (Cases endpoints), §8 (`/cases`, `/cases/:id`), §9 (diagram spec).

---

## 1. Goal recap

By the end of M2, a signed-in user can:

- Open the **Cases** tab and see all 57 OLL cases as a grid, grouped by shape, with search + Tier 1 filter chips.
- Tap any case and see its full detail: pattern diagram, algorithm, result-after-algorithm preview (with rotation applied), and Tier 1 / Tier 2 tags.
- Edit their **per-user overrides** for nickname, algorithm, result mapping (`result_case_id` + `result_rotation`), and Tier 2 tag. Clearing a field reverts that field to the global default.
- Read defaults via the override-merge logic on the backend so the frontend never has to know whether a value is global or user-specific.

Out of scope (deferred):
- Free-form user tags (`tags` / `case_tags` tables, tag CRUD UI) — M4.
- SM-2 / scheduling / grading / due queue — M3.
- Any practice or progress-state computation (`due` / `learning` / `mastered`) — M3.
- Free study filters that need progress state — M4.
- Real static page content — M5.

---

## 2. Architecture for M2

### Backend
- New migration adding `puzzle_types`, `solve_stages`, `cases`, `user_case_settings`.
- A separate **idempotent seed migration** that populates the canonical 3×3 puzzle type, the OLL stage, and all 57 cases from `initial_design/src/data.jsx`. Using a sqlx migration (rather than a one-off binary) keeps "deploy fresh DB" as a single command on Render and makes the data part of CI from M2 onward.
- Override-merge: a single SQL `LEFT JOIN cases ↔ user_case_settings` per request — `COALESCE(s.field, c.field)` per overridable field. No application-level merge logic; the SQL does it. Documented as a small helper in `backend/src/cases/merge.rs` so M3/M4 can reuse it.
- New routes module `backend/src/routes/cases.rs`: `GET /cases`, `GET /cases/:id`, `PATCH /cases/:id/settings`. Mounted alongside the existing auth router under `/api/v1`.

### Frontend
- Promote the M1 "placeholder dashboard" `HomeView` into the **app shell**: a fixed top header (display name + settings icon) and a fixed bottom **tab bar** (Practice · Cases · Progress) per `outstanding_decision.md` §1.5. Practice and Progress remain stub views for M2; only Cases lands its real content.
- New components: `PatternDiagram.vue` (Vue port of `initial_design/src/diagram.jsx`), `CaseTile.vue`, `CasesView.vue`, `CaseDetailView.vue`.
- New Pinia store `casesStore` — fetches `/cases` once on shell mount, caches the merged-and-flattened list, exposes a `byId` lookup. PATCH responses replace the cached row in place so the browser/detail stay consistent without a refetch.
- `tokens.css` already shipped in M1; no theme changes for M2.

### Reuse from M1
- `api/client.ts` axios instance + `ApiError`.
- Auth store + route guard (cases routes are `requiresAuth`).
- Rate limiter scaffolding (no new limits in M2 — the case endpoints aren't credential-adjacent).

---

## 3. Schema — M2 additions

All four tables ship in one migration. Foreign keys use `ON DELETE CASCADE` for `user_case_settings.user_id` (settings die with the user) and **no cascade** on `cases.solve_stage_id` (deleting a stage with cases is a programmer error — let it fail).

### `puzzle_types`
```
id          UUID PRIMARY KEY DEFAULT gen_random_uuid()
name        TEXT UNIQUE NOT NULL
created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
```

### `solve_stages`
```
id              UUID PRIMARY KEY DEFAULT gen_random_uuid()
puzzle_type_id  UUID NOT NULL REFERENCES puzzle_types(id)
name            TEXT NOT NULL
description     TEXT
display_order   INT NOT NULL DEFAULT 0
created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()

UNIQUE(puzzle_type_id, name)
```

### `cases`
```
id               UUID PRIMARY KEY DEFAULT gen_random_uuid()
solve_stage_id   UUID NOT NULL REFERENCES solve_stages(id)
case_number      INT NOT NULL
nickname         TEXT
algorithm        TEXT NOT NULL
result_case_id   UUID REFERENCES cases(id)   -- nullable to break the chicken/egg in seed
result_rotation  INT NOT NULL DEFAULT 0      -- 0..3 quarter-turns CW
diagram_data     JSONB NOT NULL              -- { "pattern": "LTRLXRLBR" } (see §9)
tier1_tag        TEXT NOT NULL               -- "+", "-", "L", "*"
tier2_tag        TEXT
created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()

UNIQUE(solve_stage_id, case_number)
CHECK (result_rotation BETWEEN 0 AND 3)
CHECK (tier1_tag IN ('+', '-', 'L', '*'))
```

Notes:
- `diagram_data` is JSONB but for M2 holds a single-key object `{ "pattern": "<9 chars>" }`. Wrapping in JSONB (rather than a plain TEXT column) is the design-doc decision — it lets PLL/F2L extend the field without a migration.
- `result_case_id` is nullable in the table so the seed can insert all 57 rows first and then update `result_case_id` references in a second pass. After the seed, every row has a non-null `result_case_id`. We don't enforce non-null at the column level because there's no clean way to add the constraint mid-seed without a deferrable FK dance.

### `user_case_settings`
Per-user per-case override. NULL in any column = fall back to the global default in `cases`.
```
id               UUID PRIMARY KEY DEFAULT gen_random_uuid()
user_id          UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE
case_id          UUID NOT NULL REFERENCES cases(id)
nickname         TEXT
algorithm        TEXT
result_case_id   UUID REFERENCES cases(id)
result_rotation  INT
tier2_tag        TEXT
created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()

UNIQUE(user_id, case_id)
CHECK (result_rotation IS NULL OR result_rotation BETWEEN 0 AND 3)
```

`updated_at` gets the same trigger M1 added for `users` — refactor into a shared trigger function in this migration.

### Seed migration

Separate file (`0003_seed_oll_cases.sql`) so future case-set additions don't conflict with schema changes. Steps:
1. `INSERT INTO puzzle_types (name) VALUES ('3x3') ON CONFLICT DO NOTHING`.
2. Insert OLL `solve_stage` for that puzzle type, `display_order = 0`.
3. Insert all 57 cases with `result_case_id = NULL` (matched by `(solve_stage_id, case_number)`).
4. `UPDATE cases` to set `result_case_id` from a `(case_number, result_rotation)` mapping.
5. The seed is idempotent — re-running on an existing DB is a no-op via `ON CONFLICT (solve_stage_id, case_number) DO UPDATE SET ...`. This matters because Render runs migrations on every deploy.

Source of truth: `initial_design/src/data.jsx`. Confirmed correct in `docs/outstanding_decision.md` §2.6.

---

## 4. API surface — M2 additions

Prefix `/api/v1`. All require auth.

| Method | Endpoint | Body | Returns |
|--------|----------|------|---------|
| GET | `/cases` | — | `{ cases: Case[] }` (all 57, override-merged) |
| GET | `/cases/:id` | — | `Case` (override-merged) |
| PATCH | `/cases/:id/settings` | `{ nickname?, algorithm?, result_case_id?, result_rotation?, tier2_tag? }` (each may be `null` to clear that override) | `Case` (the merged case after the write) |

### `Case` JSON shape

The merged shape returned by all three endpoints. Frontend treats every field as authoritative — no separate "global default" vs "override" exposure.

```json
{
  "id": "uuid",
  "solve_stage": "OLL",
  "puzzle_type": "3x3",
  "case_number": 1,
  "nickname": "Tie Fighter",
  "algorithm": "R U (x') U' R U l' R' U' l' U l F'",
  "result_case_id": "uuid",
  "result_case_number": 2,
  "result_rotation": 2,
  "pattern": "LTRLXRLBR",
  "tier1_tag": "*",
  "tier2_tag": "dot",
  "has_overrides": false
}
```

`has_overrides` is `true` if a `user_case_settings` row exists for this user/case with at least one non-null override field. The detail screen uses it to show a small "modified from default" affordance.

`result_case_number` is included so the frontend can render "Case 02" in the result-after-algorithm preview without a second lookup. The number is denormalized at the API layer (joined from `cases` again on the result side).

### Endpoint behaviors

**`GET /cases`**
- Single SQL query: `cases` LEFT JOIN `user_case_settings` (filtered to current user) LEFT JOIN `cases AS result_case` (on the merged `result_case_id`). Returns 57 rows.
- Sorted by `case_number ASC`.
- Cached on the frontend in `casesStore`; the next nav between Cases ↔ Detail shouldn't refetch.

**`GET /cases/:id`**
- Same merge logic, single row. 404 if the case ID doesn't exist (or isn't visible — but for M2 every case is global, so visibility is always true).

**`PATCH /cases/:id/settings`**
- Validates: `result_case_id` (if set) must reference an existing case in the same `solve_stage` (one indexed lookup; vacuous in M2 since only OLL exists, but enforced in code so PLL/F2L expansion can't introduce cross-stage corruption); `result_rotation` ∈ {0,1,2,3} when present; `nickname` and `tier2_tag` length ≤ 80; `algorithm` length ≤ 1000 (accept any text — cube notation is not parsed, so a user with non-standard notation isn't blocked).
- Upsert into `user_case_settings` keyed by `(user_id, case_id)`. `null` values explicitly clear that field (= revert to global default).
- If every field is null after the upsert, the row is deleted — keeps the table tidy and `has_overrides` becomes `false`.
- Returns the freshly-merged `Case`.
- Errors: `validation` (per-field), `not_found` (case ID unknown).

No rate limiting on case endpoints — they're auth-gated and not credential-adjacent.

---

## 5. Frontend — M2

### Routes (M2 additions)
| Path | Auth | Notes |
|------|:---:|------|
| `/` | required | now the **app shell** with tab bar; Practice tab is the default landing tab (placeholder for M3) |
| `/cases` | required | full grid + search + Tier 1 filter chips |
| `/cases/:id` | required | detail with edit mode for overrides |

The shell is rendered inside the existing `requiresAuth` route. Tab navigation rewrites the URL (`/`, `/cases`, `/progress`) so back-button works. Per `outstanding_decision.md` §1.5, settings sits behind a top-right user icon — wire that icon at the shell level, not inside individual tab views.

### Components
- **`AppShell.vue`** — fixed top bar (eyebrow `Quiet Cube`, display name on the right with click-to-`/settings`), fixed bottom tab bar with three tabs, slot for the active tab content. Replaces the M1 single-card `HomeView`.
- **`TabBar.vue`** — three icon+label tabs. Practice (placeholder), Cases (active in M2), Progress (placeholder). Active tab gets the ink color, others get `--paper-ink-muted`.
- **`PatternDiagram.vue`** — Vue port of `initial_design/src/diagram.jsx`. Props: `pattern: string` (9 chars), `size?: number` (default 120). Pure SVG output, no event handlers. Side strips (`T`/`L`/`R`/`B`) render as small rectangles outside the 3×3 face per the prototype's geometry. The prototype's unused `tone='dark'` branch is **not** ported — the prototype is light-only and dark mode is a post-MVP feature that would touch every component, not just the diagram.
- **`CaseTile.vue`** — small card for the grid: pattern at 90px, case number, nickname (if any). No grade pip in M2 (that needs progress data — M3).
- **`CasesView.vue`** — header (eyebrow "Reference", title "All cases / 57 italic"), search input, Tier 1 filter chips (`All / Learning / Dot / Known`), grouped grid by Tier 2 tag (group label rendered as `<h2>`).
- **`CaseDetailView.vue`** — back button, header (eyebrow `Case 01 · dot`, title nickname or italic "Unnamed"), pattern card, algorithm card, result-after-algorithm card (with rotation applied via the `rotatePattern` helper ported below), Tier 2 tag display. **Edit mode** swaps display fields for inputs; Save commits everything in one PATCH. Cancel reverts the draft. Tier 1 stays read-only (fixed, global).

### Pattern-rotation helper
Port `initial_design/src/data.jsx:117-132` (`rotatePatternCW`, `rotatePattern`) to a small TS module `frontend/src/lib/pattern.ts`. Pure function, easy to unit-test in Vitest.

### Pinia store
```ts
// casesStore
state: {
  list: Case[]               // all 57, sorted by case_number
  status: 'idle' | 'loading' | 'ready' | 'error'
  error: string | null
}
getters: {
  byId(id: string): Case | undefined
  byTier2(tag: string): Case[]
  groupedByTier2(): Record<string, Case[]>
}
actions: {
  ensureLoaded()              // idempotent — fetches once
  refresh()                   // forces a refetch
  updateSettings(id, patch)   // PATCH, replaces row in list on success
}
```

`ensureLoaded` is called from `AppShell.vue` on mount so the first nav into `/cases` doesn't show a flash of empty state. Splash still only waits on `/auth/me`; case loading happens after auth resolves and is allowed to be a small inline loader on the Cases tab.

### Tier 1 filter chip semantics
Five chips, mapping to the geometric shape of yellow edges on the top face. The prototype's `PRIORITY_LABELS` (Learning / Known / Not studying) are **not** carried over — those mislabeled the geometric tags as learning state. The four geometric values each get a chip because some users learn one Tier 1 group at a time (e.g. "drill all `+` cases this week, then all `*`").

| Chip | Tier 1 value | Means |
|:---:|:---:|---|
| All | — | every case |
| Dot | `*` | 0 edges oriented |
| L | `L` | 2 adjacent edges oriented |
| Line | `-` | 2 opposite edges oriented |
| Cross | `+` | all 4 edges oriented |

Chip displays the label; the value column is what the filter compares against `case.tier1_tag`.

### Search
Substring match across: case number (string-prefixed for "01" → matches case 1), nickname (case-insensitive), algorithm, Tier 2 tag label. Mirrors `screen-cases.jsx:6-21`.

### Edit-mode validation (frontend)
- `nickname` — empty string clears the override.
- `algorithm` — required if non-default; empty trim reverts to default.
- `result_case_id` — must match an existing case in the same stage (frontend validates against `casesStore.byId`).
- `result_rotation` — radio buttons `0° / 90° CW / 180° / 90° CCW`.
- `tier2_tag` — free-text field. Empty string clears the override.

Save sends only the fields the user touched; untouched fields are omitted from the PATCH body (so they aren't accidentally overwritten with the displayed merged value).

---

## 6. Security notes specific to M2

- All three endpoints are auth-gated via the existing `AuthUser` extractor — no anon access to case data in M2 (the public case browser is post-MVP per the design doc §1).
- `PATCH /cases/:id/settings` bounds the `user_case_settings.user_id` to the request's `AuthUser` — there's no path for one user to write into another user's settings even with a guessed case ID.
- `result_case_id` validation prevents a user from pointing their override at a case in a different solve stage (would render a nonsense result diagram).
- No PII added; the new tables don't carry user-identifying data beyond the FK.

---

## 7. Testing strategy

**Backend (cargo test):**
- **Test DB harness** — first piece of M2 backend work (story B0 below). `tests/common/db.rs` spins up an isolated test database per test using `sqlx::PgPool::connect_lazy`, runs migrations, and drops it on completion. Reads connection info from `TEST_DATABASE_URL` (falls back to `DATABASE_URL`). All M2 backend tests below ride on this harness; future milestones reuse it.
- Override-merge logic — integration test that `COALESCE(override, default)` returns the right value when override is null vs set, for each overridable field.
- `PATCH` upsert — integration test that the second PATCH for the same user/case updates rather than inserts; that all-null upsert deletes the row.
- Seed migration smoke test — `SELECT count(*) FROM cases` returns 57; one `puzzle_types` row, one `solve_stages` row.
- `result_case_id` cross-stage validation — error response asserted (will be vacuous until PLL ships, but the test fixture can insert a fake second stage to exercise the path).
- `algorithm` length cap (1000) — validation error asserted.

**Frontend (Vitest):**
- `rotatePattern` — round-trip test: rotating CW four times returns the original; rotating by 2 equals two CW applications.
- `casesStore.byId` / `groupedByTier2` getters with a hand-built fixture.
- `CaseDetailView` edit-mode draft handling: opening edit copies values, Cancel reverts, Save sends only changed fields.

**Manual QA:** §10 below.

---

## 8. Configuration / environment

No new env vars. M2 adds no third-party integrations.

`backend/.env.example` is unchanged. The seed migration runs automatically on `db::connect`.

---

## 9. Pattern-string spec recap

Per design doc §9, the 9-char pattern is a 3×3 grid (top-left to bottom-right) where each char encodes one cubie face on the OLL face plus its side strip:

| Char | Meaning |
|:----:|---------|
| `X` | Yellow on top of this cubie |
| `T` | Yellow on the **top** side strip of this cubie (back face's yellow sticker) |
| `L` | Yellow on the **left** side strip |
| `R` | Yellow on the **right** side strip |
| `B` | Yellow on the **bottom** side strip (front face's yellow sticker) |
| any other (e.g. `_`) | Non-yellow top sticker, no side flap |

`rotatePatternCW(p)` permutes the 9 positions and rotates the side-strip letters: `L → T → R → B → L`. `X` and non-side chars pass through unchanged.

---

## 10. "Done when" checklist (run on the deployed instance)

A literal QA script. M2 is closed when every line passes against `https://cube.nebulouscode.com` hitting `https://api.cube.nebulouscode.com`.

- [ ] Tab bar renders on `/`; Practice and Progress are clearly placeholder; Cases tab is active and lands `/cases`.
- [ ] `/cases` shows all 57 cases grouped by Tier 2 tag; each tile shows the pattern diagram, the zero-padded number (`01`–`57`), and the nickname when one exists.
- [ ] Search field filters on number, nickname, algorithm, and Tier 2 label (case-insensitive).
- [ ] Filter chips: `All` shows all 57; `Dot` shows only `*` cases; `L` shows only `L` cases; `Line` shows only `-` cases; `Cross` shows only `+` cases. Counts in the eyebrow update.
- [ ] Tapping a tile navigates to `/cases/:id`. Detail shows pattern, algorithm, Tier 1 chip, Tier 2 label, and a result-after-algorithm preview that reflects the right `result_case_id` rotated by `result_rotation`.
- [ ] Edit mode: changing nickname, then Save → tile and detail show the override; reload and the override persists; field is highlighted as "modified" via `has_overrides`.
- [ ] Edit mode: clearing a field → save reverts that field to the global default; if every field is cleared, the `user_case_settings` row is gone (verify via SQL or by checking that `has_overrides=false`).
- [ ] Edit mode: changing `result_case_id` to a valid case + a new rotation → preview updates; save persists.
- [ ] Edit mode: setting `result_case_id` to an out-of-range integer (e.g. 99) → `validation` error surfaces in the form.
- [ ] PatternDiagram renders correctly across cases that exercise all five letters (`X`/`T`/`L`/`R`/`B`) and at least one rotation that swaps side-strip letters.
- [ ] Direct nav to `/cases` while signed out → redirects to `/login?next=/cases`; logging in lands on `/cases`.
- [ ] Backend: `SELECT count(*) FROM cases` returns 57; `SELECT count(*) FROM puzzle_types` is 1; `SELECT count(*) FROM solve_stages` is 1.

---

## 11. Story list

Pairs backend + frontend per the user's principle: avoid landing endpoints with no UI. Stories are checked off as they ship.

### Test infrastructure
- [x] **B0.** sqlx integration-test harness — `backend/tests/common/mod.rs` provisions a per-test randomly-named database, runs migrations, returns a `PgPool`, and tears down on `Drop` (via a one-shot tokio runtime in a worker thread). Reads `TEST_DATABASE_URL` (refuses to fall back to `DATABASE_URL`). Smoke test in `backend/tests/db_harness.rs` connects, migrates, queries the `users` and `sessions` tables. Unblocks every B1+ test below and persists for future milestones.

### Schema + seed
- [x] **B1.** Migration `0002_cases.sql` — `puzzle_types`, `solve_stages`, `cases`, `user_case_settings` with constraints listed in §3. Reuses `set_updated_at()` from 0001 for the `user_case_settings` trigger. Integration tests in `backend/tests/cases_schema.rs` cover existence, tier1/rotation CHECKs, unique-within-stage, ON DELETE CASCADE on user_id, and the updated_at trigger.
- [x] **B2.** Seed migration `0003_seed_oll_cases.sql` — idempotent insert of 3×3 puzzle type, OLL stage, all 57 cases ported from `initial_design/src/data.jsx`, result-case backfill via a `(case_number, result_case_number)` mapping. Algorithms use Postgres dollar quoting to avoid escaping primes. Integration tests in `backend/tests/cases_seed.rs` cover: 57 OLL cases exist, exactly one puzzle_type and one stage, every case has a valid same-stage `result_case_id`, tier1_tag distribution matches the prototype (8 dots, 7 OCLL/+), and `ON CONFLICT` upserts (no duplicate insert).

### Backend endpoints (paired with frontend below)
- [x] **C1.** `cases::merge` helper module (`backend/src/cases/mod.rs`) + `GET /cases` (override-merged list). Single SQL query with COALESCE across each overridable field; `has_overrides` derived inline from `(ucs.id IS NOT NULL)`. Backend restructured: `src/lib.rs` now hosts the modules + `run()` so integration tests can call into `cases::list_for_user` directly. Tests in `backend/tests/cases_merge.rs` cover full-list, nickname override, NULL-falls-through, result_case_id override (changes `result_case_number`), cross-user isolation, and 404 on unknown ID.
- [x] **C2.** `GET /cases/:id` — same merge for a single row, 404 on unknown ID. Thin wrapper over `cases::get_for_user`.
- [x] **C3.** `PATCH /cases/:id/settings` — `Option<Option<T>>` body so we distinguish absent / null / value. `cases::update_settings` reads any existing override, applies the patch with the documented semantics, validates `result_case_id` references a same-stage case (rejects unknown UUIDs and cross-stage targets), and DELETEs the row when every field resolves to null. Returns the merged `Case`. Eight integration tests in `backend/tests/cases_settings.rs` cover create-override, null-clears, all-null deletes the row, absent-field-leaves-existing, cross-stage rejection, unknown-UUID rejection, NotFound on unknown case, and cross-user isolation.

### Frontend
- [x] **D1.** App shell + tab bar. `AppShell.vue` (PendingEmailBanner + RouterView slot + fixed bottom TabBar + floating settings icon top-right) wraps three child routes: `/` Practice (stub), `/cases` Cases, `/progress` Progress (stub). Settings remains full-bleed at `/settings`. Router restructured to use nested routes; guard now walks `to.matched` so parent meta applies to children. `HomeView.vue` removed. App.vue simplified — banner moved into the shell.
- [x] **D2.** `PatternDiagram.vue` — Vue port of `initial_design/src/diagram.jsx`, paper palette only (no `tone='dark'` per resolved §12). `frontend/src/lib/pattern.ts` with `rotatePatternCW` / `rotatePattern` helpers. 9 Vitest tests cover position permutation, side-strip letter rotation (L→T→R→B→L), zero/four/two/negative/large quarter-turn cases.
- [x] **D3.** `casesStore` Pinia store. State: `list`, `status`, `error`. Single-flight `ensureLoaded()` shares an in-flight promise across concurrent callers. Getters: `byId`, `groupedByTier2` (alphabetical, case-insensitive, with within-group `case_number` ASC). `refresh()` forces a refetch. `$reset()` for logout cleanup. PATCH/`updateSettings` lands with C3.
- [x] **D4.** `CasesView.vue` — eyebrow/title with case count, search (matches number, nickname, algorithm, raw + display Tier 2 label), 5 Tier 1 chips (All / Dot / L / Line / Cross), grouped grid of tiles with PatternDiagram + zero-padded number + nickname. Loading / error / empty states. Tile click routes to `/cases/:id` (C2/D5 land that view).
- [x] **D5.** `CaseDetailView.vue` — back button + Edit/Cancel/Save controls; case eyebrow + nickname (or italic "Unnamed"); pattern + Tier 1/Tier 2 card; algorithm card (mono); result-after-algorithm card with rotated preview using `rotatePattern`. Edit mode swaps display values for inputs (nickname text, algorithm textarea, Tier 2 text, result-case number, rotation 4-button radio). Save sends only changed fields via `casesStore.updateSettings()`; resolves the typed result-case-number to a UUID via the cached list. Validation errors surface inline. Tries cache first, falls back to `GET /cases/:id` (handles 404 → not-found state). Routed at `/cases/:id` as a top-level full-bleed route (no tab bar).

### QA
- [ ] **E1.** Walk §10 on the deployed instance. Update `outstanding_decisions_auth.md` and `TODO.md` with anything that surfaces.

---

## 12. Notes / open items

- **Tier 1 filter chip semantics.** *Resolved.* Five chips — `All / Dot / L / Line / Cross` — mapping to the geometric values `*`, `L`, `-`, `+`. Some users drill one Tier 1 group at a time, so all four shape codes get a chip. Per §5.
- **Tier 2 group order.** *Resolved (alphabetical for now).* Tier 2 group headers on the Cases page are sorted alphabetically by tag name (`awkward_shape` first → `W_shapes` last). The user may revisit with a hand-tuned cubing-friendly order later; if so, that's a small follow-up adding `tier2_display_order INT` to `cases` (or a `tier2_tags` lookup table) without changing the API shape.
- **Algorithm validation.** *Resolved.* Accept any text, length ≤ 1000 chars. The prototype accepts any text (the user is the one running it), and a user with non-standard notation shouldn't be blocked.
- **`result_case_id` "same stage" check.** *Resolved (yes).* The PATCH validates that the target case is in the same `solve_stage` as the case being edited. Vacuous in M2 (only OLL exists) but enforced in code so PLL/F2L expansion can't introduce cross-stage corruption. One indexed lookup per PATCH.
- **Display order within a Tier 2 group.** *Resolved.* Sort by `case_number ASC` within the group.
- **Test DB harness.** *Resolved.* Land in B0 as the first M2 backend ticket. All B1+ tests use it; future milestones inherit it.
- **`has_overrides` on the Cases list.** *Resolved.* Computed inline in the merge query as `(s.id IS NOT NULL)` (or equivalent COALESCE/IS NOT NULL on the join), so no per-case extra row read.
- **PatternDiagram dark tone.** *Resolved (drop it).* Verified in the prototype: `tone='dark'` is a dead branch — no caller ever passes it. The Vue port omits the prop and ships a single paper palette. Real dark mode is post-MVP and would touch every component.

---

## 13. Looking back

A retrospective pass added after the milestone shipped. Fill in honestly —
the value of these notes is the friction they capture, not a victory lap.

- **What I'd do differently if I started this milestone today:**
    - tier2_tag being a single text value did not stick and was a miscommunication on my part. I wanted to have a list from the get-go but didn't realize it wasn't setup as a list in the db until M04. 
    - Alternative cases is still unused but the next big change will be setting up PLL so I think it's worthwhile architecture even if it hasn't been used yet.

- **Surprises during execution:**
    - The pattern of Option<Option<T>> for handling json patch-bodies (absent/null-to-clear/value-to-set) was useful and reused constantly. It was the right call early on.
    -  casesStore single-flight ensureLoaded() looked premature and over engineered but became useful for implementing guest mode (which is a great feature imho) 

- **Decisions that turned out to matter more than expected:**
    - DB test harness early came in handy throughout the following milestones. Glad I set it up.
    - Setting up override-merge in SQL via COALESCE not rust made the tag rework easier. If I'd put it in the backend code it'd be more difficult to maintain.

- **Decisions that turned out not to matter:**
    - alternative cases are still unused but I am sure they'll be useful eventually
    - tier1_tag CHECK constraint has never expanded or shifted so the check has yet to fire
    - Leaving the alg as a not null text field is fine. I was never going to write a parser so no real decision was made here.

- **What this milestone taught me that I'd carry into future work:**
    - Should probably build the test infra earlier. Don't wait for a feature to test it, implement the tests as I write the code.
    - Need to make sure i'm utilizing the spare tables/columns I have. Right now the puzzle type and alg type is not earning its keep. It's unused infra that may not hold up once I get to implementing it. 
