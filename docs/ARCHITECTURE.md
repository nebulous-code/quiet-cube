# Architecture

The live reference for how Quiet Cube is built today. This doc tracks
the system as it actually runs in production at
[quiet-cube.com](https://quiet-cube.com), not the original
spec — that lives in
[`milestones/00_initial_design_doc.md`](milestones/00_initial_design_doc.md)
as a historical artifact. When code changes shape, this doc changes with
it.

For the *why* behind the SRS algorithm choice, see
[`concepts/sm2_vs_anki_summary.md`](concepts/sm2_vs_anki_summary.md). For
domain background on OLL, see
[`concepts/oll_practice.md`](concepts/oll_practice.md). For per-phase
delivery context, see [`CHANGELOG.md`](CHANGELOG.md) and the milestone
docs in `milestones/`.

---

## 1. Product scope

A web-based spaced-repetition flashcard app for Rubik's cube algorithm
practice, modeled on Anki's core study loop. The MVP ships **OLL**
(Orientation of the Last Layer) on a 3×3. Schema and API are designed
around the general concept of *puzzle → stage → case* so PLL, F2L, and
other puzzles can be added post-MVP without restructuring.

### MVP features (shipped)

- Email/password registration with email verification (6-digit code) and
  Cloudflare Turnstile.
- Login / logout / forgot-password / reset-password / change-password.
- Sign-out-everywhere with current-password gate.
- Email change via `pending_email` (no typo lockout).
- Account deletion with audit trail.
- Study mode (SM-2 schedule of due cards) and free study with
  primary-shape / tag / state filters.
- 4-button grading (Fail / Hard / Good / Easy), Anki-variant SM-2.
- Per-user overrides for nickname, algorithm, result mapping
  (case + rotation), and free-form tags.
- Progress dashboard: streak, due-today, learning, mastered, not-started.
- Guest mode — full study loop runs off `localStorage`; upgrade or merge
  on sign-in folds guest data into a real account.
- Mobile-friendly web app.

### Post-MVP roadmap

Tracked in `milestones/00_initial_design_doc.md` §1 (Post-MVP) and added
to as new ideas surface. Highlights: PLL/F2L expansion, stats over time,
admin panel, dark mode, per-user timezone rollover, email reminders,
HTTP integration tests for route handlers, cold-start UX safety net.

### Out of scope (MVP)

Native mobile app, social features, sharing custom decks between users.

---

## 2. Architecture principle

The schema is *puzzle → stage → case*. Adding PLL or F2L means inserting
rows into existing tables, not creating new ones. The frontend rendering
logic is the only piece that needs to evolve for stages with different
diagram types.

---

## 3. Database schema

PostgreSQL on Neon. All migrations live in `backend/migrations/` and run
on every deploy via `sqlx::migrate!()`. Current schema reflects nine
migrations (0001–0009).

### `puzzle_types`
```
id, name, created_at
```
One row: `3x3`.

### `solve_stages`
```
id, puzzle_type_id (FK), name, description, display_order, created_at
```
One row: `OLL`.

### `cases`
Global, canonical case data. All users inherit these defaults unless
they override.
```
id              UUID PRIMARY KEY
solve_stage_id  UUID REFERENCES solve_stages(id)
case_number     INT
nickname        TEXT             -- cleared globally; users set their own
algorithm       TEXT NOT NULL
result_case_id  UUID REFERENCES cases(id)
result_rotation INT (0=none, 1=90 CW, 2=180, 3=90 CCW), CHECK 0..=3
diagram_data    JSONB            -- { "pattern": "9-char string" }
tier1_tag       TEXT NOT NULL    -- "+" | "-" | "L" | "*"
tags            TEXT[] NOT NULL DEFAULT '{}'   -- replaces tier2_tag, see §3 note
created_at
UNIQUE (solve_stage_id, case_number)
```

Note: the original spec had a single `tier2_tag TEXT` and separate
`tags` + `case_tags` tables for free-form user tags. M4 collapsed both
into a multi-valued `tags TEXT[]` on both `cases` and
`user_case_settings`.

### `users`
```
id, email UNIQUE, pending_email, display_name, password_hash,
email_verified, verification_code (6 digits), verification_code_expires
(10-min TTL), reset_code, reset_code_expires (1-hour TTL),
streak_count, last_practice_date, has_seen_onboarding,
created_at, updated_at
```

### `user_case_settings`
Per-user overrides. NULL in any column means fall back to the global
default in `cases`. Tag overrides on `tags` are array-replace (not merge).
```
user_id, case_id, nickname, algorithm,
result_case_id, result_rotation, tags TEXT[]
UNIQUE (user_id, case_id)
```

### `user_case_progress`
SM-2 data per user per case.
```
user_id, case_id,
ease_factor (default 2.5), interval_days (default 1, CHECK >= 1),
repetitions (default 0, CHECK >= 0), due_date (default CURRENT_DATE),
last_grade (NULL | 0..=3), last_reviewed,
created_at, updated_at
UNIQUE (user_id, case_id)
```

### `sessions`
```
id, user_id, token_hash UNIQUE, expires_at, revoked, created_at
INDEX (user_id, revoked)
```
JWT revocation. Logout / sign-out-all flip `revoked = TRUE`.

### `account_deletions`
Audit row written transactionally with the user row's DELETE. No FK to
`users` (the row is gone by the time this is queried).
```
id BIGSERIAL, email TEXT NOT NULL, deleted_at TIMESTAMPTZ
INDEX (deleted_at)
```

### Cascade contract
Every FK referencing `users.id` has `ON DELETE CASCADE`. A schema-
introspection test (`tests/users_cascade_schema.rs`) enforces this so a
future migration can't silently break the contract.

---

## 4. Spaced repetition algorithm (Anki variant of SM-2)

Data shape (`ease_factor`, `interval_days`, `repetitions`, `due_date`)
matches canonical SM-2; the update rule is Anki's modified version. See
`concepts/sm2_vs_anki_summary.md` for the rationale.

### Inputs

| Code | Button | Meaning |
|-----:|--------|---------|
| 0 | Fail | Failed — couldn't recall |
| 1 | Hard | Recalled with difficulty |
| 2 | Good | Recalled cleanly |
| 3 | Easy | Recalled easily |

### Update rule

```
If grade == 0 (Fail):
    repetitions = 0
    interval_days = 1
    ease_factor -= 0.20

Else:
    if repetitions == 0: interval_days = 1
    elif repetitions == 1: interval_days = 6
    else:
        Hard: interval_days = round(interval_days * 1.2)
        Good: interval_days = round(interval_days * ease_factor)
        Easy: interval_days = round(interval_days * ease_factor * 1.3)
    repetitions += 1
    Hard: ease_factor -= 0.15
    Good: unchanged
    Easy: ease_factor += 0.15

ease_factor = max(1.3, ease_factor)
due_date = today + interval_days
```

### Constants

| Constant | Value |
|----------|------:|
| Initial ease factor | 2.5 |
| Ease floor | 1.3 |
| Hard interval multiplier | 1.2 |
| Easy bonus | 1.3 |
| Fail ease delta | −0.20 |

### State thresholds (display layer)

`not_started` (no row) → `learning` (interval < 21d) → `due` (due_date
≤ today) → `mastered` (interval ≥ 21d, due > today).

### Streak rule

On each review submission, given today's date:
- `last_practice_date` is NULL → streak = 1
- prev == today → unchanged
- prev == today − 1 → streak += 1
- otherwise → streak = 1

Then `last_practice_date = today`. Day-rollover uses server UTC; per-user
timezone is post-MVP.

### Behavioral notes

- No Anki "learning steps" (1m / 10m intermediate intervals). New cards
  go straight into the schedule on first review.
- No late-review bonus.
- New cards stay `not_started` until the user explicitly grades one;
  the first review creates the `user_case_progress` row.

---

## 5. Auth design

All verification + reset flows use **6-digit numeric codes** entered into
the app, not link-clicks. Registration is protected by Cloudflare
Turnstile (managed mode, usually invisible).

Passwords hashed with **argon2id**. JWT signed **HS256** using
`JWT_SECRET`, set as **httpOnly, Secure, SameSite=Strict, Path=/** cookie
with **30-day max-age**. No refresh token; activity does not reset the
clock.

### Registration → verify → app

1. POST `/auth/register` (display_name, email, password, turnstile_token)
2. Backend verifies Turnstile, hashes password (argon2id), inserts user
   with `email_verified=false` + 6-digit `verification_code` (10-min TTL).
3. Verification email sent via Resend.
4. POST `/auth/verify-email` with the 6-digit code. On success the
   verify endpoint sets the JWT cookie itself — no separate sign-in.
5. New user is routed to `/welcome` (onboarding stub) → `/practice`.

### Login

POST `/auth/login` (email, password). Returns 403
`{ reason: "email_not_verified" }` if not yet verified — frontend
re-routes to verify with a fresh resend. Otherwise sets the cookie,
returns user identity.

### Per-request auth

Axum `AuthUser` extractor reads the cookie, decodes the JWT, validates
expiry, looks up the matching `sessions` row by `token_hash`, rejects if
`revoked=TRUE`, and injects `user_id` into the handler.

### Logout / sign-out-all

- `POST /auth/logout` — revokes the current session row, clears cookie.
- `POST /auth/sign-out-all` — requires `{ current_password }`; on
  success revokes every session for the user and clears the cookie.

### Password reset

Forgot-password issues a 6-digit `reset_code` (1-hour TTL). Reset-password
with code + new password swaps the hash, clears the code, and revokes
all existing sessions for that user (defense against pre-reset cookie
theft).

### Email change → pending_email

PATCH `/auth/me` with `{ email: "new@..." }` writes to `pending_email`
and emails a verification code there; old email keeps logging in until
the new code verifies. Prevents typo lockout.

### Account deletion

DELETE `/auth/me` with `{ current_password }`. Inside one transaction:
INSERT into `account_deletions` (email + timestamp) + DELETE from `users`
(CASCADE handles sessions / settings / progress). Cookie wiped on
response. 3 attempts/hour rate limit per user.

### Rate limits

- `register`: 10/hour per IP
- `login`: 20/min per IP
- `resend-verification`: 1/min per email or per user
- `forgot-password`: 3/hour per email
- `delete /auth/me`: 3/hour per user

### Frontend route guard

- Unauth + protected → `/login?next=<original-path>`. Login reads `next`
  and routes there.
- Auth + guest-only routes (`/login`, `/register`, `/forgot-password`,
  `/reset-password`) → `/practice`.
- Three auth states: `'anon'`, `'guest'`, `'authed'`. `'guest'` is
  admitted to all dashboard surfaces; only `'anon'` is bounced to login.
- `/welcome` re-route is suppressed once `has_seen_onboarding=TRUE`
  (or for guests, the equivalent flag on the localStorage blob).

---

## 6. API contract

All endpoints prefixed `/api/v1`. JSON in / JSON out. Auth on protected
routes via the httpOnly JWT cookie.

### Auth

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| POST | `/auth/register` | None | Optional `guest_state` field imports localStorage data into the new account |
| POST | `/auth/verify-email` | Optional | 6-digit code; sets JWT cookie on initial verification; promotes `pending_email` when authed |
| POST | `/auth/resend-verification` | Optional | 1/min limit |
| POST | `/auth/login` | None | Sets cookie, returns user identity |
| POST | `/auth/logout` | Required | Revokes current session, clears cookie |
| POST | `/auth/sign-out-all` | Required | Body `{ current_password }`; revokes all sessions |
| POST | `/auth/forgot-password` | None | Idempotent; issues 6-digit reset code |
| POST | `/auth/reset-password` | None | Body `{ email, code, new_password }`; revokes all sessions for the user |
| POST | `/auth/change-password` | Required | Body `{ current_password, new_password }`; current session unchanged |
| POST | `/auth/onboarding-complete` | Required | Flips `has_seen_onboarding=true` |
| POST | `/auth/merge-guest-state` | Required | Folds a localStorage guest blob into the calling user's rows (max-rule for SM-2 progress) |
| GET  | `/auth/me` | Required | `{ id, email, display_name, pending_email, email_verified, has_seen_onboarding }` — identity only |
| PATCH | `/auth/me` | Required | Update display_name / email; email writes to `pending_email` |
| DELETE | `/auth/me` | Required | Body `{ current_password }`; transactional audit + delete |

### Cases

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| GET | `/cases` | Optional | All cases with overrides merged when authed; globals when anon |
| GET | `/cases/:id` | Optional | Single case with overrides merged |
| PATCH | `/cases/:id/settings` | Required | Update overrides (nickname, algorithm, result mapping, tags). Tag-cap enforcement: 100 distinct tags / 1000 case-tag links per user |

### Study

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| GET | `/study/due` | Required | `{ cases, streak }` — oldest-due first |
| POST | `/study/:case_id/review` | Required | Body `{ grade: 0..=3 }`; runs SM-2 + streak update; returns `{ case, streak }` |

Free-study filtering happens on the frontend over the merged `/cases`
list — no separate backend endpoint.

### Progress

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| GET | `/progress` | Required | `{ summary: { not_started, learning, due, mastered }, total, streak }` |
| GET | `/progress/cases` | Required | Per-case progress data |

### Misc

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| GET | `/health` | None | Liveness + readiness probe. `200 {"status":"ok","database":"ok"}`, or `503 {"status":"degraded","database":"down"}` when the database probe fails |

---

## 7. Frontend routes

Vue 3 + Vite + Vue Router 5 + Pinia. Mobile-shaped layout; light mode
only for MVP.

| Route | View | Auth | Notes |
|-------|------|------|-------|
| `/` | LandingView | None | Public marketing page; authed/guest visitors auto-redirect to `/practice` |
| `/practice` | PracticeView | Authed or guest | Dashboard — streak KPI, due card, standing chips |
| `/cases` | CasesView | Authed or guest | Browser w/ search + filters (primary, tags, **state**) |
| `/cases/:id` | CaseDetailView | Authed or guest | View / edit overrides; `?from=study` enters edit mode + returns to study on save |
| `/progress` | ProgressView | Authed or guest | Per-case state breakdown |
| `/study` | StudySessionView | Authed or guest | Full-bleed; pattern → reveal → grade |
| `/free-study` | FreeStudyView | Authed or guest | Filter screen; only/any-of toggles per axis |
| `/welcome` | OnboardingView | Authed or guest | First-run only; skipped once flag set |
| `/upgrade` | GuestUpgradeScreen | Any | Guest → real account, ships the localStorage blob to the backend |
| `/settings` | SettingsView | Authed or guest | Profile, password, sessions, delete (authed); save-progress card (guest) |
| `/login` | LoginView | Guest-only | Includes "Continue as guest" link + post-delete note via `?deleted=1` |
| `/register` | RegisterView | Guest-only | Turnstile + legal footer |
| `/verify-email` | VerifyEmailView | Either | Same view for initial verify + email-change verify |
| `/forgot-password`, `/reset-password` | — | Guest-only | 6-digit code flow |
| `/about`, `/terms`, `/privacy`, `/acknowledgements` | static | None | Placeholder copy until launch |
| `/:pathMatch(.*)*` | NotFoundView | Any | CTA depends on auth state |

### Pinia stores

- `authStore` — identity, status (`'loading' | 'anon' | 'guest' | 'authed'`), guest blob mgmt, all auth actions including `deleteAccount`.
- `casesStore` — merged case list, override editing, in guest mode merges `guestGlobals` against the localStorage blob.
- `studyStore` — in-flight session queue, streak, shuffle on every session start, `repeatSession()` replays the same set with a fresh shuffle.
- `progressStore` — state-distribution + streak summary; refetches after each review.

### Styling

Vue SFCs with `<style scoped>`. No CSS-in-JS, no utility framework, no
component library. Shared design tokens live in `tokens.css` (paper
palette, type scale, radii). Light mode only for MVP.

---

## 8. Diagrams

`<PatternDiagram>` renders dynamically from a 9-character pattern string
stored as JSONB on each case. The 9 chars encode the OLL face plus side
strips (`X` = yellow on top; `T`/`L`/`R`/`B` = sticker faces; other =
non-yellow top, no flap).

The result mapping references another case (`result_case_id` +
`result_rotation` 0–3 quarter-turns CW). The frontend rotates the
rendered SVG via CSS `transform: rotate(90deg * n)` — no second image
asset needed.

This is dynamic rather than static SVGs to avoid an asset pipeline /
naming convention / re-export step every time a case representation
changes.

---

## 9. Deployment

### Services

- **Frontend** — Vue build output served as a Render static site at
  `quiet-cube.com`.
- **Backend** — Rust binary (Axum) on a Render web service, currently
  free tier (so cold starts after ~15 min idle — see Post-MVP "Cold-start
  UX safety net").
- **Database** — Neon Postgres.
- **Email** — Resend (3,000/mo permanent free tier; domain verified).
- **Captcha** — Cloudflare Turnstile (managed mode).

### Environment variables

Backend:
```
DATABASE_URL          Neon connection string
TEST_DATABASE_URL     local Postgres for integration tests; never points at prod
JWT_SECRET            random 256-bit secret
TURNSTILE_SECRET_KEY  Cloudflare Turnstile secret
RESEND_API_KEY        Resend key
EMAIL_FROM            verified sender
FRONTEND_URL          CORS whitelist
ARGON2_M_KIB / _T / _P  optional argon2 tuning (defaults match OWASP)
RUST_LOG              log level
```

Frontend:
```
VITE_API_BASE_URL          Render backend URL
VITE_TURNSTILE_SITE_KEY    public Turnstile site key
```

### CI

`.github/workflows/test.yml` runs on every push + pull request. Two
jobs: backend (Postgres service + `cargo llvm-cov` with a 95% gate over
the testable surface) and frontend (`vue-tsc --noEmit` + `vitest run`).
Report-only — failing runs email but don't block merges.

`tools/test.sh` runs the same suite locally; flags `--backend`,
`--frontend`, `--enforce` (apply the 95% gate locally).

---

## 10. API access control (current state)

- **CORS** — Axum CORS layer whitelists the deployed Vue frontend
  origin only.
- **Rate limiting** — In-process `RateLimiter` keyed per IP / email /
  user with windows tuned per endpoint (see §5).
- **CSRF** — `SameSite=Strict` + httpOnly cookie covers it for the
  current single-origin browser model.
- Post-MVP: API keys for non-browser clients, broader rate limiting,
  tower-based middleware.
