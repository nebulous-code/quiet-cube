# Milestone 1 — Auth & Accounts (Design + Stories)

The deep dive for the first milestone in `docs/milestones/README.md`. This document is the source of truth for what gets built, in what order, with what API and data shapes. Sibling milestone docs land as we kick each one off.

---

## 1. Goal recap

A user can do everything account-related against a deployed instance, end to end:

- Register → verify email (6-digit code) → land in a placeholder dashboard
- Sign in / sign out
- Forgot password → reset code → log in
- Change password while signed in
- Edit display name; change email with the `pending_email` flow + verify banner
- Sign out everywhere (with current-password confirmation)
- Resend verification code

When this milestone is closed, *no app feature works*. The Practice / Cases / Progress tabs are stubs. That's intentional — getting auth right against real infrastructure (Render, Neon, Resend, reCAPTCHA) is the whole point of M1.

---

## 2. Architecture for M1

```
┌────────────────────────┐    HTTPS / cookie   ┌──────────────────────────┐
│  Vue SPA (Render)      │ ───────────────────▶│  Axum API (Render)       │
│  Vite static build     │                     │  Rust binary             │
│  /login, /register …   │ ◀─── JSON ──────────│  /api/v1/auth/*          │
└────────────────────────┘                     └────────┬─────────────────┘
                                                        │ sqlx
                                                        ▼
                                                ┌──────────────────────┐
                                                │  Neon Postgres       │
                                                │  users, sessions     │
                                                └──────────────────────┘

   Outbound:                                            Outbound:
   reCAPTCHA v3 (Google) — token verification           Resend (REST API) — verification + reset email
```

Frontend is a separate Render static site; backend is a Render web service. Cookies are httpOnly + Secure; CORS allows only the frontend origin (and `localhost` in dev).

---

## 3. Schema — M1 subset

Only the tables auth touches. Reproduced from `Cube_Practice_Design_Doc.md` §3 for convenience; that doc remains the source of truth.

### `users`
```
id                         UUID PRIMARY KEY
email                      TEXT UNIQUE NOT NULL
pending_email              TEXT
display_name               TEXT NOT NULL
password_hash              TEXT NOT NULL
email_verified             BOOLEAN NOT NULL DEFAULT FALSE
verification_code          TEXT
verification_code_expires  TIMESTAMPTZ
reset_code                 TEXT
reset_code_expires         TIMESTAMPTZ
streak_count               INT NOT NULL DEFAULT 0
last_practice_date         DATE
created_at                 TIMESTAMPTZ NOT NULL DEFAULT NOW()
updated_at                 TIMESTAMPTZ NOT NULL DEFAULT NOW()
```

`streak_count` and `last_practice_date` are present from M1 even though nothing writes to them yet — adding columns later requires a migration; default values are harmless.

### `sessions`
```
id          UUID PRIMARY KEY
user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE
token_hash  TEXT NOT NULL UNIQUE
expires_at  TIMESTAMPTZ NOT NULL
revoked     BOOLEAN NOT NULL DEFAULT FALSE
created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
```

Index on `(user_id, revoked)` for fast sign-out-all and on `token_hash` for the per-request lookup. The session row lives for the JWT's 30-day window even if the JWT itself is invalidated client-side — we want `revoked` to be authoritative.

---

## 4. API surface

All endpoints are `POST` unless noted. Prefix `/api/v1`. Request and response bodies are JSON. Cookie-bearing endpoints set/clear `Set-Cookie: cube_session=…; HttpOnly; Secure; SameSite=Strict; Path=/`. JWT is HS256 signed with `JWT_SECRET`, 30-day expiry, claims `{ sub, sid, exp, iat }` where `sid` is the `sessions.id`.

### Errors
A single error envelope across the surface:

```json
{ "error": "<machine_code>", "message": "<human-readable, optional>", "fields": {} }
```

`fields` is populated only on validation errors. Frontend keys off `error`.

### Endpoints

| Method | Path | Auth | Body | 200 body |
|--------|------|------|------|----------|
| POST | `/auth/register` | — | `{ display_name, email, password, recaptcha_token }` | `{ id, email, display_name, email_verified: false }` |
| POST | `/auth/verify-email` | optional | `{ code, email? }` | `{ id, email, display_name, email_verified: true }` |
| POST | `/auth/resend-verification` | optional | `{ email? }` | `{}` |
| POST | `/auth/login` | — | `{ email, password }` | `{ id, email, display_name }` |
| POST | `/auth/logout` | required | `{}` | `{}` |
| POST | `/auth/sign-out-all` | required | `{ current_password }` | `{}` |
| POST | `/auth/forgot-password` | — | `{ email }` | `{}` |
| POST | `/auth/reset-password` | — | `{ email, code, new_password }` | `{}` |
| POST | `/auth/change-password` | required | `{ current_password, new_password }` | `{}` |
| GET  | `/auth/me` | required | — | `{ id, email, display_name, pending_email, email_verified }` |
| PATCH | `/auth/me` | required | `{ display_name?, email? }` | `{ id, email, display_name, pending_email, email_verified }` |

### Endpoint behaviors

**`POST /auth/register`**
- Rate-limit 10/hour per IP.
- Validate: email RFC-ish, password ≥ 8 chars, display_name 1–80 chars.
- Verify reCAPTCHA token via Google's `siteverify`. If `success === false` or `score < 0.5`, return `recaptcha_failed`.
- Hash password with argon2id (memory 19 MiB, t=2, p=1; tune in env if needed).
- Insert `users` row with `email_verified=false`, generated 6-digit `verification_code`, `verification_code_expires = now() + 10m`.
- Email the code via Resend.
- **Does not** set a JWT cookie — the user verifies first.
- Errors: `validation`, `email_in_use` (409), `recaptcha_failed` (403), `rate_limited` (429).

**`POST /auth/verify-email`**
- Two modes determined by auth presence:
  - **Unauthenticated (initial registration):** body must include `email`. Look up user by `email`. If `verification_code` matches and not expired → set `email_verified=true`, clear code fields, create a `sessions` row, set the JWT cookie, return 200.
  - **Authenticated (email change):** body has `code` only. Look up the user via `AuthUser`. Require `pending_email` to be set. If `verification_code` matches and not expired → set `email = pending_email`, clear `pending_email`, clear code fields, return 200. Cookie unchanged.
- Errors: `invalid_code`, `code_expired`, `no_pending_verification` (404 — only for the auth'd path), `validation`.

**`POST /auth/resend-verification`**
- Same dual-mode: `{ email }` if unauthenticated, no body needed if authenticated.
- Generate a fresh `verification_code`, reset `verification_code_expires`, email it.
- Per-user rate limit: 1/minute. Exceeded → 429 with `retry_after_seconds`.
- For the unauthenticated mode, if no user matches `email`, return 200 anyway (avoid enumeration).

**`POST /auth/login`**
- Look up user by email. If absent or password mismatch → `invalid_credentials` (401, generic).
- If `email_verified=false` → `email_not_verified` (403). Frontend routes to verify-email screen and triggers a resend.
- Create a `sessions` row, set the JWT cookie.
- Per-IP rate limit: 20/min on this endpoint specifically (brute-force defense).

**`POST /auth/logout`**
- Mark the current `sessions` row revoked. Clear the cookie.

**`POST /auth/sign-out-all`**
- Verify `current_password` with argon2 — wrong → 401 `invalid_password`, do nothing else.
- `UPDATE sessions SET revoked=true WHERE user_id = $1`. Clear the cookie.

**`POST /auth/forgot-password`**
- Idempotent. Always returns 200 to avoid email enumeration.
- If a user exists with that email, generate a 6-digit `reset_code`, set `reset_code_expires = now() + 1h`, email the code.
- Per-email rate limit: 3/hour.

**`POST /auth/reset-password`**
- Validate code + expiry against `users.reset_code` for the email. If invalid → `invalid_code` (400) or `code_expired` (400, generic enough to not enumerate).
- Hash new password with argon2, store, clear `reset_code` and `reset_code_expires`.
- **Also revoke all existing sessions for the user** — a password reset implies the previous password may be compromised.
- Does not auto-login. User goes to the login screen.

**`POST /auth/change-password`**
- Verify `current_password`. Wrong → 401.
- Validate `new_password` (≥ 8 chars; reject if equal to current).
- Hash and store. Cookie unchanged. Other sessions stay live unless followed up with sign-out-all.

**`GET /auth/me`**
- Returns identity only — no stats. (See `outstanding_decisions_auth.md` item E.)
- 401 if no valid session.

**`PATCH /auth/me`**
- `display_name` writes through immediately.
- If `email` differs from the current `users.email`:
  - Reject if the new email is already in `users.email` for any user → `email_in_use` (409).
  - Write to `users.pending_email`. Generate a new `verification_code`, set expiry, email the code to `pending_email`.
  - Don't touch `users.email` or `users.email_verified`. The user stays logged in with the original email valid for login.
- Returns the same shape as `GET /auth/me`.

---

## 5. Email templates

Three messages, sent through Resend's REST API. Markdown-style copy below; HTML versions get the same content with the app's paper aesthetic in inline styles.

### 5.1 Verification — initial registration
Subject: **Verify your Quiet Cube email**

Body:
> Welcome to Quiet Cube.
>
> Your 6-digit verification code is: **`{code}`**
>
> This code expires in 10 minutes. If you didn't sign up, you can ignore this email.

### 5.2 Verification — email change
Subject: **Confirm your new Quiet Cube email**

Body:
> You requested to change the email on your Quiet Cube account to **{new_email}**.
>
> Your 6-digit verification code is: **`{code}`**
>
> This code expires in 10 minutes. Until you confirm, sign-in will continue to work with your previous email.

### 5.3 Password reset
Subject: **Reset your Quiet Cube password**

Body:
> Someone requested a password reset for your Quiet Cube account.
>
> Your 6-digit reset code is: **`{code}`**
>
> This code expires in 1 hour. If you didn't request a reset, you can ignore this email — your password hasn't changed.

All emails: text/plain primary, HTML alternate, `From: EMAIL_FROM` (per `.env`).

---

## 6. Frontend

### Routes (M1 subset)
| Path | Auth | Notes |
|------|:---:|------|
| `/login` | guest-only | redirects to `/` if logged in |
| `/register` | guest-only | reCAPTCHA v3 invisible |
| `/verify-email` | both | dual-mode (initial + email change) |
| `/forgot-password` | guest-only | |
| `/reset-password` | guest-only | accepts `?email=` query for prefill |
| `/` | required | placeholder dashboard ("M1 done — app coming next milestone") |
| `/settings` | required | account + security sections |
| `/about`, `/terms`, `/privacy` | none | placeholder static pages with copy TBD |

### Pinia stores (M1)
- `authStore` — `{ user: User | null, status: 'loading' | 'guest' | 'authed' }`. Actions: `bootstrap()` (calls `GET /auth/me`), `login`, `logout`, `register`, `verify`, `resendVerification`, `forgotPassword`, `resetPassword`, `changePassword`, `signOutAll`, `updateProfile`.

No other stores in M1.

### Components needing real implementations
From `initial_design/src/`:
- `SplashScreen` — wired to `bootstrap()` rather than a hardcoded timer
- `LoginScreen`, `RegisterScreen`, `VerifyEmailScreen`, `ForgotPasswordScreen`, `ResetPasswordScreen`
- `SettingsScreen`, `SecuritySection`
- `AuthShell`, `AuthHeader`, `Field`, `PasswordField`, `PrimaryCTA`, `TextLink`
- `Avatar`, `SettingsRow`, `Eyebrow`

Not yet:
- `OnboardingScreen` (designer's deeper work; ship as a 1-screen stub or skip per user's note in the auth-decisions doc item 9)
- `GuestUpgradeScreen` (M6)
- All practice/cases/progress screens

### Route guard (Vue Router `beforeEach`)
```
if route requires auth and !authStore.user:
    redirect to /login?next=<requested_path>
if route is guest-only and authStore.user:
    redirect to /
otherwise:
    proceed
```

While `authStore.status === 'loading'` (initial bootstrap), show the splash — guards don't fire until status resolves.

### Splash behavior
- On app mount, call `authStore.bootstrap()`. Until it resolves: render splash.
- Enforce ≥ 800ms display so the splash doesn't flicker on a fast response.
- On 200 → set `status='authed'`, route to original `next` or `/`. On 401 → set `status='guest'`, route to `/login` (or honor a guest-only deep link).

### Email-change banner
Rendered globally by the layout shell when `authStore.user.pending_email` is set:

> "Verify your new email **{pending_email}** to switch addresses. **Resend code →**"

Resend triggers `authStore.resendVerification()` (auth'd mode).

---

## 7. Security notes specific to M1

- Cookies: `HttpOnly; Secure; SameSite=Strict; Path=/`. The `Strict` choice may break OAuth-style return flows later but we have no return flows in M1, so it's the safe default.
- argon2id parameters tunable via env (`ARGON2_M_KIB`, `ARGON2_T`, `ARGON2_P`).
- All codes are uniformly random 6-digit strings (`000000`–`999999`). Compare in constant time.
- Password reset revokes all sessions (M1 §4 above).
- reCAPTCHA score threshold (0.5) configurable via env.
- All write endpoints validate inputs server-side; the frontend's client-side validation is for UX only.
- Logging via `tracing` — never log passwords, codes, or full JWTs (log only `sub` and `sid`).
- Rate limits enforced per the table in §4. Implementation: an in-process tower-http layer is fine for MVP — Redis can come later.

---

## 8. Testing strategy

Three layers, all expected before the milestone closes:

1. **Rust unit tests** — argon2 wrapper, JWT helpers, rate-limiter, code generation, email-template rendering. Pure functions; no DB.
2. **Rust integration tests** — every endpoint, against a throwaway Postgres (testcontainers or a CI service Postgres). Cover happy paths, validation errors, rate-limit boundaries, and the dual-mode behaviors (verify-email, resend-verification).
3. **Manual end-to-end checklist on the deployed instance** — the explicit "done when" checklist below in §10. Run after each significant deploy.

E2E browser tests (Playwright/Cypress) are deferred per `outstanding_decision.md` §3.3.

---

## 9. Configuration / environment

### Backend (Render web service)
```
DATABASE_URL          # Neon connection string
JWT_SECRET            # 256-bit random
RECAPTCHA_SECRET_KEY  # Google reCAPTCHA v3 secret
RECAPTCHA_MIN_SCORE   # default 0.5
RESEND_API_KEY        # Resend API key
EMAIL_FROM            # verified sender, e.g. "Quiet Cube <noreply@mail.nebulouscode.com>"
FRONTEND_URL          # CORS allowlist + email link generation
RUST_LOG              # default "info,oll=debug"
ARGON2_M_KIB          # default 19456
ARGON2_T              # default 2
ARGON2_P              # default 1
```

### Frontend (Render static site)
```
VITE_API_BASE_URL          # Render backend URL
VITE_RECAPTCHA_SITE_KEY    # Google reCAPTCHA v3 public key
```

---

## 10. "Done when" checklist (run on the deployed instance)

A literal QA script. M1 is closed when every line passes against `https://app.<domain>` hitting `https://api.<domain>`.

- [ ] Splash → `/login` for an anonymous visitor
- [ ] `/register` rejects: invalid email format, password < 8 chars, missing display name, duplicate email
- [ ] Successful registration sends a verification email within ~30 seconds; email contains a 6-digit code, copy matches §5.1
- [ ] `/verify-email`: code-entry boxes; correct code → app; wrong code → `invalid_code`; expired code → `code_expired`
- [ ] After verify, user is logged in (no separate login step) and lands on the placeholder dashboard
- [ ] Refresh the page → still logged in
- [ ] Sign out → `/login`; refresh → still on `/login`
- [ ] `/login` rejects: wrong password, unverified account (returns to verify screen with a fresh resend)
- [ ] "Resend code" on the verify screen sends a new code; rapid-fire resends are 429'd after 1 in 60 s
- [ ] `/forgot-password` sends a reset code; reset code works exactly once; expired reset code is rejected
- [ ] After password reset, sessions on a second browser tab/device are revoked (refresh → `/login`)
- [ ] `/settings`: change display name → reflected in avatar/header
- [ ] `/settings`: change email → banner appears with `pending_email`; old email still logs in; verification code arrives at the *new* email; entering the code swaps `users.email` and clears the banner
- [ ] `/settings/security`: change password requires the current password; succeeds with valid input; current session stays live
- [ ] `/settings/security`: sign out everywhere prompts for current password; wrong → `invalid_password`; right → all sessions on all devices end
- [ ] Direct nav to `/settings` while signed out → redirects to `/login?next=/settings`; logging in lands on `/settings`
- [ ] Direct nav to `/login` while signed in → redirects to `/`
- [ ] reCAPTCHA: registration with a tampered/missing token → `recaptcha_failed`
- [ ] Rate limits: 11th register from same IP within an hour → 429; 21st login from same IP within a minute → 429

---

## 11. Story list

Listed in dependency-friendly order. Each item is sized as a single PR or short stack. Story IDs are stable; check off in this file as work lands.

### Pre-work / infrastructure (no code)
- [ ] **A1.** Provision Neon project + database; capture `DATABASE_URL`. Apply IP allowlist if Render egress is fixed.
- [ ] **A2.** Register Resend account, add and verify the sending domain (1–2 business day SLA — start now).
- [ ] **A3.** Register reCAPTCHA v3 site at the Google admin console; capture site key and secret key.
- [ ] **A4.** Create Render frontend (static site) and backend (web service); wire env vars with placeholders. Custom domain optional for M1.

### Backend foundations
- [ ] **B1.** Rust + Axum scaffold: `cargo new`, dependency graph, health check at `GET /api/v1/health`, `.env` loading with `dotenvy`, tower-http layers (CORS, trace, timeout). CI builds and deploys to Render. Depends on A4.
- [ ] **B2.** sqlx setup: `sqlx-cli` migrations directory, connection pool, fixtures helper for tests. Depends on B1, A1.
- [ ] **B3.** Migrations 0001 — `users` and `sessions` tables per §3, indexes included. Depends on B2.
- [ ] **B4.** Argon2id helper module with parameters from env; unit tests for hash/verify roundtrip. No deps beyond B1.
- [ ] **B5.** JWT helper module (sign, verify, claims struct); cookie helper (build, clear); `AuthUser` Axum extractor that also looks up `sessions` and rejects revoked. Depends on B3, B4.
- [ ] **B6.** Resend client wrapper (REST via `reqwest`); email-template module with the three §5 templates; integration test against Resend in dev. Depends on B1, A2.
- [ ] **B7.** reCAPTCHA verifier (POST to Google `siteverify`, score check). Depends on B1, A3.
- [ ] **B8.** Generic error envelope, `AppError` enum (`thiserror`), validation helper using the `validator` crate. Depends on B1.
- [x] **B9.** In-process rate limiter (tower-http or custom Tower middleware) keyed by IP and by user. Depends on B1.

### Backend endpoints
- [ ] **C1.** `POST /auth/register` — depends on B3, B4, B6, B7, B8, B9. Integration tests cover happy path, validation, dup email, captcha failure, rate limit.
- [ ] **C2.** `POST /auth/verify-email` (dual-mode) — depends on B5, B6, C1. Integration tests cover unauth'd verification (sets cookie), auth'd email-change (promotes pending_email), invalid/expired codes.
- [ ] **C3.** `POST /auth/resend-verification` (dual-mode + rate limit) — depends on B6, B9, C1.
- [ ] **C4.** `POST /auth/login` + `POST /auth/logout` — depends on B5, B9. Integration tests cover happy path, wrong creds, unverified, rate limit.
- [ ] **C5.** `POST /auth/forgot-password` + `POST /auth/reset-password` — depends on B6, B9. Reset must also revoke all sessions for the user.
- [ ] **C6.** `POST /auth/change-password` — depends on B5.
- [ ] **C7.** `POST /auth/sign-out-all` (with current-password gate) — depends on B5.
- [ ] **C8.** `GET /auth/me` + `PATCH /auth/me` (with `pending_email` flow + email-in-use check) — depends on B5, B6.

### Frontend foundations
- [ ] **D1.** Vue + Vite + Vue Router + Pinia + TypeScript scaffold. CI builds and deploys to Render. Tokens.css ported from `initial_design/src/ui.jsx` (`paper`, `fonts`). ESLint + Prettier wired up. Depends on A4.
- [ ] **D2.** API client (axios) with `withCredentials: true`, error envelope unwrapping, error-code → user-message mapping. Depends on D1.
- [ ] **D3.** `authStore` (Pinia) with all the actions enumerated in §6. Depends on D2.
- [ ] **D4.** Layout shell + tab bar (placeholders for Practice/Cases/Progress); `AuthShell` and `AuthHeader` from prototype ported. Depends on D1.
- [ ] **D5.** Splash screen with the bootstrap flow (≥ 800ms minimum, awaits `/auth/me`). Depends on D3.
- [ ] **D6.** Route guard with `next` query param, plus guest-only redirects. Depends on D3.

### Frontend views
- [ ] **D7.** Login + Register screens (with reCAPTCHA v3 invisible token submission). Depends on D3, C1, C4.
- [ ] **D8.** Verify-email screen (6-digit code entry, resend) — both initial and email-change modes share the same component. Depends on D3, C2, C3.
- [ ] **D9.** Forgot-password + reset-password screens. Depends on D3, C5.
- [ ] **D10.** Settings — Account section (display name, email change with banner). Depends on D3, C8.
- [ ] **D11.** Settings — Security section (change password, sign out, sign out everywhere). Depends on D3, C6, C7.
- [ ] **D12.** Email-change pending banner in the layout shell. Depends on D4, D8.
- [ ] **D13.** Placeholder dashboard at `/`. Single short copy line — "more app coming in milestone 2." Depends on D4.
- [x] **D14.** Placeholder static pages: `/about`, `/terms`, `/privacy`, `/acknowledgements`. Real content per `TODO.md`. Depends on D4.

### Verification
- [ ] **E1.** Walk the §10 checklist on the deployed instance. Update `outstanding_decisions_auth.md` and `TODO.md` with anything that surfaces.

---

## 12. Notes / open items

- The "what does the dashboard look like in M1" answer (§6, D13) is a single placeholder card with no logic. Designer involvement deferred to M5 polish.
- Onboarding (auth-decisions doc item 9) is not in M1. The designer will tackle it later. Stories deliberately omit it.
- If we decide later to flip rate-limiter implementation to Redis-backed, the in-process version (B9) stays as a fallback / dev-mode default.
- §10 is the first source of truth for what "M1 closed" means. If the user disagrees with anything there, it's the easiest place to amend.
 

---

## 13. Looking back

A retrospective pass added after the milestone shipped. Fill in honestly —
the value of these notes is the friction they capture, not a victory lap.

- **What I'd do differently if I started this milestone today:**
    - Skip the recaptcha through google and use turnstile going forward. I picked something I was familiar with but there was a better more private option to use that suits me better.
    - Build out one end to end smoke test with Playwright. I deferred this decision and come launch day there were bugs that would've been caught by that.
    - [ ] Turnstile has some quirks that I should document outside of this repo. 
- **Surprises during execution:**
    - I've yet to see anyone change their email. This is a nice feature to have but it definitely could've been left off for the mvp.
    - SameSite=Strict looked like an obvious and safe choice however it is making the dev environment and potential mobile app development non-trivial to solve for. This decision will likely get undone long term.

- **Decisions that turned out to matter more than expected:**
    - having JWT as a bearer of sid and trusting sessions.revoked seemed like it was defensible but it's now load-bearing for M07. Not bad just a big outcome of a small decision.
    - glad I added streak_count and last_practice_date even though they weren't implemented until later. Made it easy to skip a migration step
    - SameSite=Strict above is going to carry weight through the project. Didn't expect something seemingly secure to matter that much.

- **Decisions that turned out not to matter:**
    - Tuning never happened for argon2. Probably could've hardcoded those.
    - Redis later was the right call but it's yet to arrive and probably never will for this project.
    - Splash minimum was a nice touch. I like seeing the logo. But probably unnecessary.

- **What this milestone taught me that I'd carry into future work:**
   - Defensive columns in the database (NOT NULL DEFAULT something safe) are free at creation time and help maintain the database
   - Probably would try and implement playwright going forward for stuff like this. At least to make sure everything's loading right on deployment
   - Could've genericized the captcha/bot protection secret so changing to Turnstile didn't require ENV renames.
   - Chose to move away from Google's recaptcha for privacy reasons. It came up in the design of the privacy policy and was a nice easy win to remove some leagalese from the privacy policy.
